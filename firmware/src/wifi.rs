use crate::{
    clock::monotonic_us,
    error::{FirmwareError, Result},
    storage::{QueuedWifi, StorageService},
};
use blelog_format::{
    capture::{
        CaptureConfig, Profile, WifiMeta, WIFI_FCS_INCLUDED, WIFI_MAX_LEN, WIFI_METADATA_ONLY,
    },
    wire::LoggerState,
};
use esp_idf_svc::{
    eventloop::EspSystemEventLoop, hal::peripherals::Peripherals, sys, wifi::WifiDriver,
};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        OnceLock,
    },
    thread,
    time::Duration,
};

struct Context {
    storage: StorageService,
    accepting: AtomicBool,
    in_callback: AtomicBool,
    session_low: AtomicU32,
    session_high: AtomicU32,
    window: AtomicU32,
    sequence: AtomicU32,
    scratch: *mut QueuedWifi,
}
// Scratch is accessed only by the callback holding in_callback. All other fields
// are synchronized; allocation lives for the process lifetime, as does the driver.
unsafe impl Send for Context {}
unsafe impl Sync for Context {}
static CONTEXT: OnceLock<Context> = OnceLock::new();
static INITIALIZED: OnceLock<Result<()>> = OnceLock::new();

fn check(rc: i32) -> Result<()> {
    if rc == sys::ESP_OK {
        Ok(())
    } else {
        Err(FirmwareError::Wifi(rc))
    }
}
pub fn initialize(storage: &StorageService) -> Result<()> {
    *INITIALIZED.get_or_init(|| {
        let peripherals = Peripherals::take().map_err(|e| FirmwareError::Wifi(e.code()))?;
        let event_loop = EspSystemEventLoop::take().map_err(|e| FirmwareError::Wifi(e.code()))?;
        let driver = WifiDriver::new(peripherals.modem, event_loop, None)
            .map_err(|e| FirmwareError::Wifi(e.code()))?;
        check(unsafe { sys::esp_wifi_set_mode(sys::wifi_mode_t_WIFI_MODE_NULL) })?;
        let scratch = unsafe {
            sys::heap_caps_aligned_calloc(
                core::mem::align_of::<QueuedWifi>(),
                1,
                core::mem::size_of::<QueuedWifi>(),
                sys::MALLOC_CAP_INTERNAL | sys::MALLOC_CAP_8BIT,
            )
        }
        .cast::<QueuedWifi>();
        if scratch.is_null() {
            return Err(FirmwareError::QueueClosed);
        }
        // Every bit pattern in QueuedWifi is valid; zeroed scratch contains no references.
        let context = Context {
            storage: storage.clone(),
            accepting: AtomicBool::new(false),
            in_callback: AtomicBool::new(false),
            session_low: AtomicU32::new(0),
            session_high: AtomicU32::new(0),
            window: AtomicU32::new(0),
            sequence: AtomicU32::new(0),
            scratch,
        };
        if CONTEXT.set(context).is_err() {
            unsafe {
                sys::heap_caps_free(scratch.cast());
            }
            return Err(FirmwareError::InvalidState);
        }
        check(unsafe { sys::esp_wifi_set_promiscuous_rx_cb(Some(on_packet)) })?;
        // Raw IDF calls below own the single driver for the rest of this boot.
        core::mem::forget(driver);
        Ok(())
    })
}
pub fn configure(storage: &StorageService, mut config: CaptureConfig) -> Result<CaptureConfig> {
    initialize(storage)?;
    let country = [config.country[0], config.country[1], 0];
    check(unsafe { sys::esp_wifi_set_country_code(country.as_ptr().cast(), false) })?;
    let mut permitted = sys::wifi_country_t::default();
    check(unsafe { sys::esp_wifi_get_country(&mut permitted) })?;
    let mut mask = 0u16;
    for channel in permitted.schan..permitted.schan.saturating_add(permitted.nchan) {
        if (1..=13).contains(&channel) {
            mask |= 1 << (channel - 1);
        }
    }
    if config.channel_mask == 0 {
        config.channel_mask = mask;
    }
    if config.channel_mask == 0 || config.channel_mask & !mask != 0 {
        return Err(FirmwareError::Bounds);
    }
    let mut filter = sys::wifi_promiscuous_filter_t {
        filter_mask: match config.profile {
            Profile::Discovery => sys::WIFI_PROMIS_FILTER_MASK_MGMT,
            Profile::Raw => {
                sys::WIFI_PROMIS_FILTER_MASK_MGMT
                    | sys::WIFI_PROMIS_FILTER_MASK_DATA
                    | sys::WIFI_PROMIS_FILTER_MASK_CTRL
                    | sys::WIFI_PROMIS_FILTER_MASK_MISC
            }
        },
    };
    if config.failed_fcs {
        filter.filter_mask |= sys::WIFI_PROMIS_FILTER_MASK_FCSFAIL;
    }
    check(unsafe { sys::esp_wifi_set_promiscuous_filter(&filter) })?;
    let control = sys::wifi_promiscuous_filter_t {
        filter_mask: sys::WIFI_PROMIS_CTRL_FILTER_MASK_ALL,
    };
    check(unsafe { sys::esp_wifi_set_promiscuous_ctrl_filter(&control) })?;
    Ok(config)
}
pub fn session(session: u64) {
    if let Some(ctx) = CONTEXT.get() {
        ctx.session_low.store(session as u32, Ordering::Relaxed);
        ctx.session_high
            .store((session >> 32) as u32, Ordering::Relaxed);
        ctx.sequence.store(0, Ordering::Relaxed);
    }
}
pub fn start() -> Result<()> {
    check(unsafe { sys::esp_wifi_start() })
}
pub fn tune(channel: u8) -> Result<()> {
    pause()?;
    check(unsafe {
        sys::esp_wifi_set_channel(channel, sys::wifi_second_chan_t_WIFI_SECOND_CHAN_NONE)
    })
}
pub fn listen(window: u32) -> Result<()> {
    let ctx = CONTEXT.get().ok_or(FirmwareError::InvalidState)?;
    ctx.window.store(window, Ordering::Release);
    ctx.accepting.store(true, Ordering::Release);
    if let Err(error) = check(unsafe { sys::esp_wifi_set_promiscuous(true) }) {
        ctx.accepting.store(false, Ordering::Release);
        return Err(error);
    }
    Ok(())
}
pub fn pause() -> Result<()> {
    let Some(ctx) = CONTEXT.get() else {
        return Ok(());
    };
    ctx.accepting.store(false, Ordering::Release);
    let result = check(unsafe { sys::esp_wifi_set_promiscuous(false) });
    while ctx.in_callback.load(Ordering::Acquire) {
        thread::sleep(Duration::from_millis(1));
    }
    result
}
pub fn stop() -> Result<()> {
    if CONTEXT.get().is_none() {
        return Ok(());
    }
    let paused = pause();
    let stopped = check(unsafe { sys::esp_wifi_stop() });
    paused.and(stopped)
}
unsafe extern "C" fn on_packet(
    buffer: *mut core::ffi::c_void,
    kind: sys::wifi_promiscuous_pkt_type_t,
) {
    let Some(ctx) = CONTEXT.get() else {
        return;
    };
    let entry_window = ctx.window.load(Ordering::Acquire);
    if !ctx.accepting.load(Ordering::Acquire) {
        return;
    }
    if ctx
        .in_callback
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
        .is_err()
    {
        ctx.storage.stats.radio[2].fetch_add(1, Ordering::Relaxed);
        return;
    }
    // No early returns inside this section: always release the callback barrier.
    if ctx.accepting.load(Ordering::Acquire)
        && ctx.window.load(Ordering::Acquire) == entry_window
        && !buffer.is_null()
    {
        let time = monotonic_us();
        let sequence = ctx.sequence.fetch_add(1, Ordering::Relaxed);
        let stats = &ctx.storage.stats;
        stats.radio[0].fetch_add(1, Ordering::Relaxed);
        let packet = &*buffer.cast::<sys::wifi_promiscuous_pkt_t>();
        let rx = &packet.rx_ctrl;
        let original = rx.sig_len() as usize;
        let length = if kind == sys::wifi_promiscuous_pkt_type_t_WIFI_PKT_MISC {
            0
        } else {
            original
        };
        if kind > sys::wifi_promiscuous_pkt_type_t_WIFI_PKT_MISC
            || original > WIFI_MAX_LEN
            || !(1..=13).contains(&rx.channel())
        {
            stats.radio[5].fetch_add(1, Ordering::Relaxed);
        } else if matches!(ctx.storage.state(), LoggerState::Full) {
            stats.radio[3].fetch_add(1, Ordering::Relaxed);
        } else if !matches!(
            ctx.storage.state(),
            LoggerState::Capturing | LoggerState::Stopping
        ) {
            stats.radio[4].fetch_add(1, Ordering::Relaxed);
        } else {
            let frame = &mut *ctx.scratch;
            frame.meta = WifiMeta {
                session_id: u64::from(ctx.session_low.load(Ordering::Relaxed))
                    | (u64::from(ctx.session_high.load(Ordering::Relaxed)) << 32),
                sequence,
                monotonic_us: time,
                window_id: entry_window,
                hardware_timestamp_us: rx.timestamp(),
                original_len: original as u16,
                captured_len: length as u16,
                channel: rx.channel() as u8,
                secondary_channel: rx.secondary_channel() as u8,
                rssi_dbm: rx.rssi() as i8,
                packet_type: kind as u8,
                rate: rx.rate() as u8,
                sig_mode: rx.sig_mode() as u8,
                mcs: rx.mcs() as u8,
                bandwidth: rx.cwb() as u8,
                aggregation: rx.aggregation() as u8,
                rx_state: rx.rx_state() as u8,
                flags: if length == 0 {
                    WIFI_METADATA_ONLY
                } else {
                    WIFI_FCS_INCLUDED
                },
            };
            core::ptr::copy_nonoverlapping(
                packet.payload.as_ptr(),
                frame.payload.as_mut_ptr(),
                length,
            );
            if ctx.storage.wifi.try_send_copy(frame) {
                stats.radio[6].fetch_max(ctx.storage.wifi.len() as u32, Ordering::Relaxed);
            } else {
                stats.radio[2].fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    ctx.in_callback.store(false, Ordering::Release);
}
