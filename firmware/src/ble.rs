use std::sync::{
    atomic::{AtomicBool, AtomicU32, Ordering},
    Arc,
};

use blelog_format::record::{
    ReportKind, DATA_STATUS_UNKNOWN, EXTENDED_PROPERTIES_UNKNOWN, PHY_UNKNOWN, TX_POWER_UNKNOWN,
};
use blelog_format::wire::{LoggerState, StartCapture};
use esp32_nimble::{enums::AdvType, BLEDevice, BLEScan};
use heapless::Vec as HeaplessVec;

use crate::{
    clock::monotonic_us,
    error::{FirmwareError, Result},
    storage::{QueuedObservation, StorageService},
};

const EXT_PROP_SCAN_RESPONSE: u8 = 0x08;
const SCAN_SLICE_MS: i32 = 1_000;
static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

pub fn initialize() {
    let _ = BLEDevice::take();
}

pub fn run_scan(
    storage: StorageService,
    sequence: Arc<AtomicU32>,
    session_id: u64,
    settings: StartCapture,
) -> Result<()> {
    let result = esp_idf_svc::hal::task::block_on(async {
        let device = BLEDevice::take();
        let mut scan = BLEScan::new();
        scan.active_scan(settings.scan_mode == blelog_format::ScanMode::Active)
            .filter_duplicates(false)
            .interval(settings.interval_ms)
            .window(settings.window_ms);

        while !STOP_REQUESTED.load(Ordering::Acquire) {
            // ESP-IDF 5.5.3's external discovery cancel clears the callback
            // state without delivering DISC_COMPLETE. BLEScan waits for that
            // event, so bounded public scans provide deterministic shutdown.
            scan.start(device, SCAN_SLICE_MS, |device, data| {
                // Sequence and timestamp are assigned before any copying or queue work.
                let sequence_number = sequence.fetch_add(1, Ordering::Relaxed);
                let timestamp = monotonic_us();
                storage.stats.received.fetch_add(1, Ordering::Relaxed);

                let payload_bytes = data.payload();
                if payload_bytes.len() > 255 {
                    // Never store a silently truncated payload as if it were complete.
                    storage.stats.queue_drops.fetch_add(1, Ordering::Relaxed);
                    return None::<()>;
                }
                let mut payload = HeaplessVec::<u8, 255>::new();
                if payload.extend_from_slice(payload_bytes).is_err() {
                    storage.stats.queue_drops.fetch_add(1, Ordering::Relaxed);
                    return None::<()>;
                }

                let address = device.addr();
                let (report_kind, legacy_event_type, extended_properties) = match device.adv_type()
                {
                    AdvType::Ind => (ReportKind::Advertisement, 0, EXTENDED_PROPERTIES_UNKNOWN),
                    AdvType::DirectInd => {
                        (ReportKind::Advertisement, 1, EXTENDED_PROPERTIES_UNKNOWN)
                    }
                    AdvType::ScanInd => (ReportKind::Advertisement, 2, EXTENDED_PROPERTIES_UNKNOWN),
                    AdvType::NonconnInd => {
                        (ReportKind::Advertisement, 3, EXTENDED_PROPERTIES_UNKNOWN)
                    }
                    AdvType::ScanResponse => {
                        (ReportKind::ScanResponse, 4, EXTENDED_PROPERTIES_UNKNOWN)
                    }
                    AdvType::Extended(properties) => {
                        let kind = if properties & EXT_PROP_SCAN_RESPONSE != 0 {
                            ReportKind::ScanResponse
                        } else {
                            ReportKind::Advertisement
                        };
                        (kind, 0xFF, u16::from(properties))
                    }
                };

                storage.submit(QueuedObservation {
                    session_id,
                    sequence: sequence_number,
                    monotonic_us: timestamp,
                    address: address.as_be_bytes(),
                    address_type: address.addr_type() as u8,
                    rssi_dbm: device.rssi(),
                    report_kind,
                    data_status: DATA_STATUS_UNKNOWN,
                    primary_phy: device.prim_phy() as u8,
                    secondary_phy: device.sec_phy().map_or(PHY_UNKNOWN, |phy| phy as u8),
                    advertising_sid: device.sid(),
                    tx_power_dbm: TX_POWER_UNKNOWN,
                    legacy_event_type,
                    extended_properties,
                    periodic_interval: device.periodic_itvl(),
                    payload,
                });
                None::<()>
            })
            .await?;
        }
        Ok::<(), esp32_nimble::BLEError>(())
    });

    match result {
        Ok(_) => Ok(()),
        Err(error) => {
            log::error!("BLE scan failed: {error:?}");
            storage.set_state(LoggerState::Error);
            Err(FirmwareError::Ble)
        }
    }
}

pub fn clear_stop_request() {
    STOP_REQUESTED.store(false, Ordering::Release);
}

pub fn request_stop() {
    STOP_REQUESTED.store(true, Ordering::Release);
}
