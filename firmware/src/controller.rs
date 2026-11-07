use std::{
    sync::{
        atomic::{AtomicU32, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc,
    },
    thread,
};

use blelog_format::{
    capture::{CaptureConfig, Radios},
    record::{ClockSync, ScanMode, SessionStart},
    wire::{LoggerState, StartCapture, MAX_SCAN_MS},
};

use crate::{
    ble,
    clock::{monotonic_us, ClockState},
    error::{FirmwareError, Result},
    hardware::HardwareInfo,
    storage::{StorageService, StorageSnapshot},
};

pub const FIELD_DEFAULTS: StartCapture = StartCapture {
    scan_mode: ScanMode::Active,
    interval_ms: 100,
    window_ms: 100,
};
#[cfg(not(feature = "radio-selftest"))]
pub const AUTOSTART_CAPTURE: bool = true;

struct ScanRequest {
    session_id: u64,
    config: CaptureConfig,
    seed: u32,
    done: SyncSender<()>,
}

pub struct Controller {
    pub storage: StorageService,
    pub hardware: HardwareInfo,
    pub boot_id: u32,
    pub scan_settings: StartCapture,
    pub capture_config: CaptureConfig,
    pub boot_config: CaptureConfig,
    clock: ClockState,
    sequence: Arc<AtomicU32>,
    session_id: u64,
    scanner: Option<Receiver<()>>,
    worker: SyncSender<ScanRequest>,
}

impl Controller {
    pub fn new(storage: StorageService, hardware: HardwareInfo) -> Result<Self> {
        let boot_config = crate::radio::load_config().unwrap_or_else(|error| {
            log::warn!("invalid boot capture config: {error}; using defaults");
            CaptureConfig::default()
        });
        // Reserve the internal stack once, before Wi-Fi initialization fragments
        // the remaining heap. STOP waits for a session, not task destruction.
        let sequence = Arc::new(AtomicU32::new(0));
        let worker_sequence = sequence.clone();
        let worker_storage = storage.clone();
        let (worker, requests) = mpsc::sync_channel::<ScanRequest>(1);
        thread::Builder::new()
            .name("radio-scanner".into())
            .stack_size(16 * 1024)
            .spawn(move || {
                while let Ok(request) = requests.recv() {
                    let _ = crate::radio::run(
                        worker_storage.clone(),
                        worker_sequence.clone(),
                        request.session_id,
                        request.config,
                        request.seed,
                    );
                    let _ = worker_storage.finish_session(request.session_id, monotonic_us());
                    #[cfg(feature = "radio-selftest")]
                    log::info!(
                        "SELFTEST scanner minimum stack remaining: {} bytes",
                        unsafe {
                            esp_idf_svc::sys::uxTaskGetStackHighWaterMark(core::ptr::null_mut())
                        }
                    );
                    let _ = request.done.send(());
                }
            })
            .map_err(|_| FirmwareError::QueueClosed)?;
        Ok(Self {
            storage,
            hardware,
            boot_id: unsafe { esp_idf_svc::sys::esp_random() },
            scan_settings: FIELD_DEFAULTS,
            capture_config: boot_config,
            boot_config,
            clock: ClockState::default(),
            sequence,
            session_id: 0,
            scanner: None,
            worker,
        })
    }

    #[cfg(not(feature = "radio-selftest"))]
    pub fn autostart(&mut self) {
        let Ok(snapshot) = self.storage.snapshot() else {
            return;
        };
        if AUTOSTART_CAPTURE
            && snapshot.state == LoggerState::Ready
            && snapshot.free_bytes() >= 1024
        {
            if let Err(error) = self.start_radios(self.boot_config) {
                log::error!("capture autostart failed: {error}");
            }
        }
    }

    pub fn start_capture(&mut self, settings: StartCapture) -> Result<()> {
        self.start_radios(CaptureConfig {
            radios: Radios::Ble,
            ble: settings,
            ..CaptureConfig::default()
        })
    }

    pub fn save_config(&mut self, config: CaptureConfig) -> Result<()> {
        if self.scanner.is_some()
            || !matches!(
                self.storage.state(),
                LoggerState::Ready | LoggerState::Full | LoggerState::CorruptTail
            )
        {
            return Err(FirmwareError::InvalidState);
        }
        config.validate().map_err(|_| FirmwareError::Bounds)?;
        if config.radios != Radios::Ble {
            crate::wifi::configure(&self.storage, config)?;
        }
        crate::radio::save_config(config)?;
        self.boot_config = config;
        Ok(())
    }

    pub fn start_radios(&mut self, mut config: CaptureConfig) -> Result<()> {
        config.validate().map_err(|_| FirmwareError::Bounds)?;
        let settings = config.ble;
        if self.scanner.is_some() || self.storage.state() != LoggerState::Ready {
            return Err(FirmwareError::InvalidState);
        }
        if settings.interval_ms == 0
            || settings.interval_ms > MAX_SCAN_MS
            || settings.window_ms == 0
            || settings.window_ms > settings.interval_ms
        {
            return Err(FirmwareError::Bounds);
        }
        if config.radios != Radios::Ble {
            config = crate::wifi::configure(&self.storage, config)?;
        }
        let session_id = (u64::from(unsafe { esp_idf_svc::sys::esp_random() }) << 32)
            | u64::from(unsafe { esp_idf_svc::sys::esp_random() });
        let start_monotonic_us = monotonic_us();
        let (wall_clock_unix_ms, clock_monotonic_us) = self.clock.mapping();
        let start = SessionStart {
            session_id,
            boot_id: self.boot_id,
            start_monotonic_us,
            wall_clock_unix_ms,
            clock_monotonic_us,
            scan_mode: settings.scan_mode,
            scan_interval_ms: settings.interval_ms,
            scan_window_ms: settings.window_ms,
            extended_adv_enabled: true,
            firmware_version: env!("CARGO_PKG_VERSION").as_bytes(),
        };
        self.storage.stats.reset_radio();
        self.storage.begin_session(start)?;
        let seed = unsafe { esp_idf_svc::sys::esp_random() };
        if let Err(error) = self.storage.configure_session(session_id, seed, config) {
            let _ = self.storage.finish_session(session_id, monotonic_us());
            return Err(error);
        }
        ble::clear_stop_request();
        self.sequence.store(0, Ordering::Relaxed);
        self.session_id = session_id;
        self.scan_settings = settings;
        self.capture_config = config;
        let (done, finished) = mpsc::sync_channel(1);
        match self.worker.send(ScanRequest {
            session_id,
            config,
            seed,
            done,
        }) {
            Ok(()) => {
                self.scanner = Some(finished);
                Ok(())
            }
            Err(_) => {
                self.storage.finish_session(session_id, monotonic_us())?;
                self.session_id = 0;
                Err(FirmwareError::Ble)
            }
        }
    }

    pub fn stop_capture(&mut self) -> Result<()> {
        let Some(scanner) = self.scanner.take() else {
            return if matches!(
                self.storage.state(),
                LoggerState::Ready | LoggerState::Full | LoggerState::CorruptTail
            ) {
                Ok(())
            } else {
                Err(FirmwareError::InvalidState)
            };
        };
        let state = self.storage.state();
        if !matches!(state, LoggerState::Full | LoggerState::Error) {
            self.storage.set_state(LoggerState::Stopping);
        }
        ble::request_stop();
        scanner.recv().map_err(|_| FirmwareError::QueueClosed)?;
        // The worker has stopped both radios and drained their callbacks/queues.
        let session_id = self.session_id;
        self.session_id = 0;
        self.storage.finish_session(session_id, monotonic_us())
    }

    pub fn set_clock(&self, unix_ms: i64) -> Result<u64> {
        let monotonic_us = monotonic_us();
        match self.storage.state() {
            LoggerState::Capturing => self.storage.clock_sync(ClockSync {
                session_id: self.session_id,
                unix_ms,
                monotonic_us,
            })?,
            LoggerState::Ready => {}
            _ => return Err(FirmwareError::InvalidState),
        }
        self.clock.set(unix_ms, monotonic_us);
        Ok(monotonic_us)
    }

    pub fn snapshot(&self) -> Result<StorageSnapshot> {
        self.storage.snapshot()
    }
}
