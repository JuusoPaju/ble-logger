use std::{
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
};

use blelog_format::{
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
pub const AUTOSTART_CAPTURE: bool = true;

pub struct Controller {
    pub storage: StorageService,
    pub hardware: HardwareInfo,
    pub boot_id: u32,
    pub scan_settings: StartCapture,
    clock: ClockState,
    sequence: Arc<AtomicU32>,
    session_id: u64,
    scanner: Option<JoinHandle<()>>,
}

impl Controller {
    pub fn new(storage: StorageService, hardware: HardwareInfo) -> Self {
        Self {
            storage,
            hardware,
            boot_id: unsafe { esp_idf_svc::sys::esp_random() },
            scan_settings: FIELD_DEFAULTS,
            clock: ClockState::default(),
            sequence: Arc::new(AtomicU32::new(0)),
            session_id: 0,
            scanner: None,
        }
    }

    pub fn autostart(&mut self) {
        let Ok(snapshot) = self.storage.snapshot() else {
            return;
        };
        if AUTOSTART_CAPTURE
            && snapshot.state == LoggerState::Ready
            && snapshot.free_bytes() >= 1024
        {
            if let Err(error) = self.start_capture(FIELD_DEFAULTS) {
                log::error!("capture autostart failed: {error}");
            }
        }
    }

    pub fn start_capture(&mut self, settings: StartCapture) -> Result<()> {
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
        self.storage.begin_session(start)?;
        ble::clear_stop_request();
        self.sequence.store(0, Ordering::Relaxed);
        self.session_id = session_id;
        self.scan_settings = settings;
        let storage = self.storage.clone();
        let sequence = Arc::clone(&self.sequence);
        match thread::Builder::new()
            .name("ble-scanner".into())
            .stack_size(16 * 1024)
            .spawn(move || {
                let _ = ble::run_scan(storage.clone(), sequence, session_id, settings);
                // This also closes sessions stopped by FULL or a storage-triggered
                // cancellation. The control-path finish call is idempotent.
                let _ = storage.finish_session(session_id, monotonic_us());
            }) {
            Ok(handle) => {
                self.scanner = Some(handle);
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
        let _ = scanner.join(); // callback is complete when the dedicated task exits
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
