use crate::{
    ble,
    clock::monotonic_us,
    error::{FirmwareError, Result},
    storage::StorageService,
    wifi,
};
use blelog_format::{
    capture::{CaptureConfig, CaptureRecord, ChannelCursor, Radios, CONFIG_LEN},
    wire::LoggerState,
};
use esp_idf_svc::sys;
use std::{
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

pub fn run(
    storage: StorageService,
    sequence: Arc<AtomicU32>,
    session: u64,
    config: CaptureConfig,
    seed: u32,
) -> Result<()> {
    let mut cursor =
        ChannelCursor::new(config.channel_mask.max(1), seed).map_err(|_| FirmwareError::Bounds)?;
    let mut window = 0u32;
    let mut random = seed.max(1);
    let mut wifi_started = false;
    wifi::session(session);
    let result = (|| {
        while !stopping(&storage) {
            if config.radios != Radios::Wifi {
                window = window.wrapping_add(1);
                marker(&storage, session, window, 1, 0, true)?;
                let start = monotonic_us();
                storage.stats.active_radio.store(1, Ordering::Relaxed);
                ble::run_scan(
                    storage.clone(),
                    sequence.clone(),
                    session,
                    config.ble,
                    config.ble_ms,
                )?;
                add_time(&storage, 7, start);
                storage.stats.active_radio.store(0, Ordering::Relaxed);
                if !stopping(&storage) {
                    marker(&storage, session, window, 1, 0, false)?;
                }
            }
            if config.radios != Radios::Ble && !stopping(&storage) {
                let switch = monotonic_us();
                wifi::start()?;
                wifi_started = true;
                add_time(&storage, 9, switch);
                for _ in 0..config.channels_per_burst {
                    if stopping(&storage) {
                        break;
                    }
                    let channel = cursor.next_channel();
                    window = window.wrapping_add(1);
                    let switch = monotonic_us();
                    wifi::tune(channel)?;
                    add_time(&storage, 9, switch);
                    marker(&storage, session, window, 2, channel, true)?;
                    wifi::listen(window)?;
                    storage.stats.active_radio.store(2, Ordering::Relaxed);
                    storage
                        .stats
                        .active_channel
                        .store(channel as u32, Ordering::Relaxed);
                    let start = monotonic_us();
                    random ^= random << 13;
                    random ^= random >> 17;
                    random ^= random << 5;
                    let jitter = (random % 41) as i32 - 20;
                    let end = Instant::now()
                        + Duration::from_millis((i32::from(config.wifi_dwell_ms) + jitter) as u64);
                    while Instant::now() < end && !stopping(&storage) {
                        thread::sleep(Duration::from_millis(5));
                    }
                    wifi::pause()?;
                    add_time(&storage, 8, start);
                    storage.stats.active_radio.store(0, Ordering::Relaxed);
                    storage.stats.active_channel.store(0, Ordering::Relaxed);
                    if !stopping(&storage) {
                        marker(&storage, session, window, 2, channel, false)?;
                    }
                }
                let switch = monotonic_us();
                wifi::stop()?;
                wifi_started = false;
                add_time(&storage, 9, switch);
            }
        }
        Ok(())
    })();
    let cleanup = if wifi_started { wifi::stop() } else { Ok(()) };
    storage.stats.active_radio.store(0, Ordering::Relaxed);
    storage.stats.active_channel.store(0, Ordering::Relaxed);
    let result = result.and(cleanup);
    if let Err(error) = result {
        // FULL during a window-marker append is an ordinary terminal condition.
        if storage.state() != LoggerState::Full {
            storage.stats.radio[10].fetch_add(1, Ordering::Relaxed);
            storage.set_state(LoggerState::Error);
            log::error!("radio scheduler stopped: {error}");
        }
    }
    result
}
fn stopping(storage: &StorageService) -> bool {
    ble::stop_requested() || !matches!(storage.state(), LoggerState::Capturing)
}
fn add_time(storage: &StorageService, index: usize, start: u64) {
    // Milliseconds internally avoid the 71-minute overflow of u32 microseconds.
    storage.stats.radio[index]
        .fetch_add(((monotonic_us() - start) / 1000) as u32, Ordering::Relaxed);
}
fn marker(
    storage: &StorageService,
    session_id: u64,
    window_id: u32,
    radio: u8,
    channel: u8,
    begin: bool,
) -> Result<()> {
    let result = storage.capture_record(CaptureRecord::Window {
        session_id,
        window_id,
        radio,
        channel,
        begin,
        monotonic_us: monotonic_us(),
    });
    // STOP may race the state check immediately before a window marker. This
    // is a normal stop, not a scheduler fault.
    if matches!(result, Err(FirmwareError::InvalidState)) && ble::stop_requested() {
        Ok(())
    } else {
        result
    }
}

fn nvs_open() -> Result<sys::nvs_handle_t> {
    let rc = unsafe { sys::nvs_flash_init() };
    if rc != sys::ESP_OK {
        return Err(FirmwareError::Config(rc));
    }
    let mut handle = 0;
    let rc = unsafe {
        sys::nvs_open(
            c"blelog".as_ptr(),
            sys::nvs_open_mode_t_NVS_READWRITE,
            &mut handle,
        )
    };
    if rc != sys::ESP_OK {
        return Err(FirmwareError::Config(rc));
    }
    Ok(handle)
}
pub fn load_config() -> Result<CaptureConfig> {
    let handle = nvs_open()?;
    let mut bytes = [0u8; CONFIG_LEN];
    let mut length = bytes.len();
    let rc = unsafe {
        sys::nvs_get_blob(
            handle,
            c"capture".as_ptr(),
            bytes.as_mut_ptr().cast(),
            &mut length,
        )
    };
    unsafe {
        sys::nvs_close(handle);
    }
    if rc == sys::ESP_ERR_NVS_NOT_FOUND {
        return Ok(CaptureConfig::default());
    }
    if rc != sys::ESP_OK {
        return Err(FirmwareError::Config(rc));
    }
    CaptureConfig::decode(&bytes[..length]).map_err(|_| FirmwareError::Bounds)
}
pub fn save_config(config: CaptureConfig) -> Result<()> {
    let mut bytes = [0u8; CONFIG_LEN];
    config
        .encode(&mut bytes)
        .map_err(|_| FirmwareError::Bounds)?;
    let handle = nvs_open()?;
    let mut rc = unsafe {
        sys::nvs_set_blob(
            handle,
            c"capture".as_ptr(),
            bytes.as_ptr().cast(),
            bytes.len(),
        )
    };
    if rc == sys::ESP_OK {
        rc = unsafe { sys::nvs_commit(handle) };
    }
    unsafe {
        sys::nvs_close(handle);
    }
    if rc == sys::ESP_OK {
        Ok(())
    } else {
        Err(FirmwareError::Config(rc))
    }
}
