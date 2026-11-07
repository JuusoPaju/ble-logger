mod ble;
mod clock;
mod controller;
mod error;
mod hardware;
mod protocol;
mod queue;
mod radio;
#[cfg(feature = "radio-selftest")]
mod selftest;
mod stats;
mod storage;
mod wifi;

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
    log::info!("starting dual-radio logger");
    log::info!("reset reason: {}", unsafe {
        esp_idf_svc::sys::esp_reset_reason()
    });
    let usb = protocol::UsbSerialJtag::install()?;
    diagnostic("USB installed");

    let state = Arc::new(AtomicU8::new(LoggerState::Booting as u8));
    let stats = Arc::new(AtomicStats::default());
    let hardware = HardwareInfo::detect();
    diagnostic("hardware detected");
    let storage = match StorageService::start(Arc::clone(&stats), Arc::clone(&state)) {
        Ok(storage) => storage,
        Err(error) => {
            log::error!("storage startup failed: {error}");
            protocol::run_error(usb, error)
        }
    };
    diagnostic("storage recovered");
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
    log::info!("recovered state   : {:?}", snapshot.state);
    if hardware.warning_flags != 0 || snapshot.partition_size != EXPECTED_PARTITION_SIZE {
        log::warn!(
            "hardware/layout mismatch: flags=0x{:04x}, partition={}",
            hardware.warning_flags,
            snapshot.partition_size
        );
    }

    // Recovery is complete before the BLE host is initialized or scanning starts.
    if let Err(error) = ble::initialize() {
        log::error!("BLE startup failed: {error}");
        protocol::run_error(usb, error);
    }
    diagnostic("BLE initialized");
    let mut controller = match Controller::new(storage, hardware) {
        Ok(controller) => controller,
        Err(error) => {
            log::error!("controller startup failed: {error}");
            protocol::run_error(usb, error)
        }
    };
    diagnostic("controller initialized");
    #[cfg(not(feature = "radio-selftest"))]
    controller.autostart();
    #[cfg(feature = "radio-selftest")]
    if let Err(error) = selftest::run(&mut controller) {
        log::error!("RADIO SELFTEST FAILED: {error}");
    }
    diagnostic("autostart returned");
    state.store(controller.storage.state() as u8, Ordering::Release);
    protocol::run(usb, &mut controller)
}

pub fn diagnostic(message: &str) {
    log::info!(
        "startup: {message}; internal free={}, largest={}",
        unsafe {
            esp_idf_svc::sys::heap_caps_get_free_size(
                esp_idf_svc::sys::MALLOC_CAP_INTERNAL | esp_idf_svc::sys::MALLOC_CAP_8BIT,
            )
        },
        unsafe {
            esp_idf_svc::sys::heap_caps_get_largest_free_block(
                esp_idf_svc::sys::MALLOC_CAP_INTERNAL | esp_idf_svc::sys::MALLOC_CAP_8BIT,
            )
        }
    );
}
