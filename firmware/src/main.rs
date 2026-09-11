mod ble;
mod clock;
mod controller;
mod error;
mod hardware;
mod protocol;
mod stats;
mod storage;

use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};

use blelog_format::wire::LoggerState;
use controller::Controller;
use hardware::{HardwareInfo, EXPECTED_PARTITION_SIZE};
use stats::AtomicStats;
use storage::StorageService;

fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    let state = Arc::new(AtomicU8::new(LoggerState::Booting as u8));
    let stats = Arc::new(AtomicStats::default());
    let hardware = HardwareInfo::detect();
    let storage = StorageService::start(Arc::clone(&stats), Arc::clone(&state))?;
    let snapshot = storage.snapshot()?;

    log::info!("BLE Logger v{}", env!("CARGO_PKG_VERSION"));
    log::info!("chip              : {}", hardware.chip_model);
    log::info!(
        "base MAC          : {:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        hardware.base_mac[0],
        hardware.base_mac[1],
        hardware.base_mac[2],
        hardware.base_mac[3],
        hardware.base_mac[4],
        hardware.base_mac[5]
    );
    log::info!("flash detected    : {} bytes", hardware.flash_size);
    log::info!("psram detected    : {} bytes", hardware.psram_size);
    log::info!("blelog partition  : {} bytes", snapshot.partition_size);
    log::info!("recovered prefix  : {} bytes", snapshot.valid_bytes);
    log::info!("valid records     : {}", snapshot.record_count);
    log::info!("free bytes        : {}", snapshot.free_bytes());
    if hardware.warning_flags != 0 || snapshot.partition_size != EXPECTED_PARTITION_SIZE {
        log::warn!(
            "hardware/layout mismatch: flags=0x{:04x}, partition={}",
            hardware.warning_flags,
            snapshot.partition_size
        );
    }

    // Recovery is complete before the BLE host is initialized or scanning starts.
    ble::initialize();
    let usb = protocol::UsbSerialJtag::install()?;
    let mut controller = Controller::new(storage, hardware);
    controller.autostart();
    state.store(controller.storage.state() as u8, Ordering::Release);
    protocol::run(usb, &mut controller)
}
