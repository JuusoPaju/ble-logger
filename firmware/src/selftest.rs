//! Explicit, bounded UART lifecycle test for a board with only one data cable.
//! Appends test sessions; never erases the log or changes saved boot settings.
use crate::{
    controller::Controller,
    error::{FirmwareError, Result},
};
use blelog_format::{
    capture::{CaptureConfig, Profile},
    wire::LoggerState,
};
use std::{
    thread,
    time::{Duration, Instant},
};

pub fn run(controller: &mut Controller) -> Result<()> {
    let state = controller.snapshot()?.state;
    if state != LoggerState::Ready {
        log::warn!("RADIO SELFTEST NOT RUN: recovered state {state:?}; preserve/download the log before any explicit erase");
        return Err(FirmwareError::InvalidState);
    }
    log::warn!("RADIO SELFTEST: keep USB/power connected; 20 sessions then STOPPED");
    // Exercise STOP in BLE windows, Wi-Fi windows and near handoff boundaries.
    let durations = [100, 950, 1020, 1450, 2050];
    for index in 0..20 {
        let config = CaptureConfig {
            profile: if index % 2 == 0 {
                Profile::Discovery
            } else {
                Profile::Raw
            },
            ..CaptureConfig::default()
        };
        log::info!("SELFTEST {} start {:?}", index + 1, config.profile);
        controller.start_radios(config)?;
        thread::sleep(Duration::from_millis(durations[index % durations.len()]));
        let start = Instant::now();
        controller.stop_capture()?;
        controller.stop_capture()?; // STOP must remain idempotent.
        let snapshot = controller.snapshot()?;
        let stats = controller.storage.stats.radio_snapshot();
        log::info!("SELFTEST {} stopped: state={:?}, stop_ms={}, wifi_stored={}, drops={}, failures={}, prefix={}",
            index + 1, snapshot.state, start.elapsed().as_millis(), stats.0[1], stats.0[2], stats.0[10], snapshot.valid_bytes);
        if snapshot.state != LoggerState::Ready || stats.0[10] != 0 {
            return Err(FirmwareError::InvalidState);
        }
    }
    log::warn!("RADIO SELFTEST PASSED: 20 starts / 40 stops; capture STOPPED");
    Ok(())
}
