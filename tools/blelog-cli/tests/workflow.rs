use std::{collections::HashMap, fs, path::Path};

use anyhow::{bail, Result};
use blelog_cli::{
    export::{export_file, ExportFormat},
    pull::{partial_path, pull},
    verify::verify_file,
    RequestClient,
};
use blelog_format::{
    encode_record,
    record::{
        BleObservation, ClockSync, Record, ReportKind, ScanMode, SessionStart, DATA_STATUS_UNKNOWN,
        EXTENDED_PROPERTIES_UNKNOWN, FORMAT_VERSION, LEGACY_EVENT_UNKNOWN, MAX_RECORD_LEN,
        PHY_UNKNOWN, SID_UNKNOWN, TX_POWER_UNKNOWN, UNIX_MS_UNKNOWN,
    },
    transport::message,
    wire::{
        decode_dump_read_request, decode_u32, encode_dump_begin, encode_dump_read_response, Info,
        LoggerState,
    },
};

struct FakeDevice {
    log: Vec<u8>,
    transfer: u32,
    failures: HashMap<u32, u8>,
    ended: bool,
}

impl FakeDevice {
    fn new(log: Vec<u8>) -> Self {
        Self {
            log,
            transfer: 0xCAFE_BABE,
            failures: HashMap::new(),
            ended: false,
        }
    }

    fn info(&self) -> Vec<u8> {
        let mut out = [0u8; 128];
        let info = Info {
            record_format: FORMAT_VERSION,
            state: LoggerState::Ready,
            scan_mode: ScanMode::Active,
            extended_adv_enabled: true,
            flash_size: 16 * 1024 * 1024,
            psram_size: 8 * 1024 * 1024,
            partition_size: 0xCF0000,
            valid_bytes: self.log.len() as u32,
            free_bytes: 0xCF0000 - self.log.len() as u32,
            record_count: blelog_format::scan_stream(&self.log).record_count,
            base_mac: [1, 2, 3, 4, 5, 6],
            warning_flags: 0,
            scan_interval_ms: 100,
            scan_window_ms: 100,
            chip_model: "ESP32-S3",
            firmware_version: "0.1.0",
        };
        let len = info.encode(&mut out).unwrap();
        out[..len].to_vec()
    }
}

impl RequestClient for FakeDevice {
    fn request(&mut self, message_type: u8, payload: &[u8]) -> Result<Vec<u8>> {
        match message_type {
            message::INFO => Ok(self.info()),
            message::STOP_CAPTURE => Ok(vec![]),
            message::DUMP_BEGIN => {
                let mut out = [0u8; 16];
                let records = blelog_format::scan_stream(&self.log).record_count;
                encode_dump_begin(self.transfer, self.log.len() as u32, records, &mut out)?;
                Ok(out.to_vec())
            }
            message::DUMP_READ => {
                let (transfer, offset, length) = decode_dump_read_request(payload)?;
                if transfer != self.transfer {
                    bail!("wrong transfer");
                }
                let attempts = self.failures.entry(offset).or_default();
                *attempts += 1;
                if *attempts == 1 {
                    bail!("synthetic transient read failure");
                }
                let end = (offset as usize + usize::from(length)).min(self.log.len());
                let mut out = vec![0; 10 + end - offset as usize];
                let len = encode_dump_read_response(
                    transfer,
                    offset,
                    &self.log[offset as usize..end],
                    &mut out,
                )?;
                out.truncate(len);
                Ok(out)
            }
            message::DUMP_END => {
                if decode_u32(payload)? != self.transfer {
                    bail!("wrong transfer");
                }
                self.ended = true;
                Ok(vec![])
            }
            _ => bail!("unexpected command"),
        }
    }
}

#[test]
fn interrupted_pull_trims_only_truncated_record_and_retries_chunks() {
    let log = sample_log();
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("walk.blelog");
    let part = partial_path(&output);
    let first_len = blelog_format::decode_record(&log).unwrap().total_len;
    fs::write(&part, &log[..first_len + 10]).unwrap();

    let mut device = FakeDevice::new(log.clone());
    let summary = pull(&mut device, &output, true, 37).unwrap();
    assert_eq!(summary.bytes, log.len() as u64);
    assert_eq!(summary.observations, 2);
    assert!(device.ended);
    assert_eq!(fs::read(output).unwrap(), log);
    assert!(!part.exists());
}

#[test]
fn resume_refuses_crc_corruption() {
    let log = sample_log();
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("walk.blelog");
    let part = partial_path(&output);
    let first_len = blelog_format::decode_record(&log).unwrap().total_len;
    let mut corrupt = log[..first_len].to_vec();
    corrupt[20] ^= 1;
    fs::write(&part, corrupt).unwrap();
    let error = pull(&mut FakeDevice::new(log), &output, true, 128)
        .unwrap_err()
        .to_string();
    assert!(error.contains("cannot resume"));
}

#[test]
fn jsonl_and_csv_exports_have_the_verified_observation_count() {
    let log = sample_log();
    let directory = tempfile::tempdir().unwrap();
    let raw = directory.path().join("walk.blelog");
    let jsonl = directory.path().join("walk.jsonl");
    let csv = directory.path().join("walk.csv");
    fs::write(&raw, log).unwrap();
    assert_eq!(verify_file(&raw).unwrap().observation_count, 2);
    assert_eq!(export_file(&raw, &jsonl, ExportFormat::Jsonl).unwrap(), 2);
    assert_eq!(export_file(&raw, &csv, ExportFormat::Csv).unwrap(), 2);
    let json = fs::read_to_string(jsonl).unwrap();
    assert_eq!(json.lines().count(), 2);
    assert!(json.contains("\"unix_timestamp_ms\":1789000000000"));
    assert!(json.contains("\"address\":\"aa:bb:cc:dd:ee:ff\""));
    assert!(json.contains("\"raw_payload_hex\":\"020106\""));
    let csv_text = fs::read_to_string(csv).unwrap();
    assert_eq!(csv_text.lines().count(), 3); // header plus two rows
}

fn sample_log() -> Vec<u8> {
    let mut bytes = Vec::new();
    append(
        &mut bytes,
        Record::SessionStart(SessionStart {
            session_id: 7,
            boot_id: 9,
            start_monotonic_us: 1_000_000,
            wall_clock_unix_ms: UNIX_MS_UNKNOWN,
            clock_monotonic_us: 0,
            scan_mode: ScanMode::Active,
            scan_interval_ms: 100,
            scan_window_ms: 100,
            extended_adv_enabled: true,
            firmware_version: b"test",
        }),
    );
    append(
        &mut bytes,
        observation(7, 0, 1_000_000, &[0x02, 0x01, 0x06]),
    );
    append(
        &mut bytes,
        Record::ClockSync(ClockSync {
            session_id: 7,
            unix_ms: 1_789_000_000_000,
            monotonic_us: 1_000_000,
        }),
    );
    append(&mut bytes, observation(7, 1, 1_005_000, &[]));
    bytes
}

fn observation<'a>(
    session_id: u64,
    sequence: u32,
    monotonic_us: u64,
    payload: &'a [u8],
) -> Record<'a> {
    Record::BleObservation(BleObservation {
        session_id,
        sequence,
        monotonic_us,
        address: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
        address_type: 1,
        rssi_dbm: -60,
        report_kind: ReportKind::Advertisement,
        data_status: DATA_STATUS_UNKNOWN,
        primary_phy: PHY_UNKNOWN,
        secondary_phy: PHY_UNKNOWN,
        advertising_sid: SID_UNKNOWN,
        tx_power_dbm: TX_POWER_UNKNOWN,
        legacy_event_type: LEGACY_EVENT_UNKNOWN,
        extended_properties: EXTENDED_PROPERTIES_UNKNOWN,
        periodic_interval: 0,
        payload,
    })
}

fn append(output: &mut Vec<u8>, record: Record<'_>) {
    let mut buffer = [0u8; MAX_RECORD_LEN];
    let len = encode_record(&record, &mut buffer).unwrap();
    output.extend_from_slice(&buffer[..len]);
}

#[allow(dead_code)]
fn assert_file(_path: &Path) {}
