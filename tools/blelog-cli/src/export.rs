use std::{
    collections::HashMap,
    fs::{self, File},
    io::{BufWriter, Write},
    path::Path,
};

use anyhow::{Context, Result};
use blelog_format::record::{
    decode_record, DecodedRecord, ReportKind, DATA_STATUS_UNKNOWN, EXTENDED_PROPERTIES_UNKNOWN,
    LEGACY_EVENT_UNKNOWN, PHY_UNKNOWN, SID_UNKNOWN, TX_POWER_UNKNOWN, UNIX_MS_UNKNOWN,
};
use serde::Serialize;

use crate::verify::verify_bytes;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Jsonl,
    Csv,
}

#[derive(Debug, Serialize)]
struct ObservationRow {
    session_id: String,
    sequence: u32,
    monotonic_timestamp_us: u64,
    unix_timestamp_ms: Option<i64>,
    address: String,
    address_type: Option<&'static str>,
    rssi_dbm: i8,
    report_kind: &'static str,
    data_status: Option<u8>,
    legacy_event_type: Option<u8>,
    extended_properties: Option<u16>,
    primary_phy: Option<u8>,
    secondary_phy: Option<u8>,
    advertising_sid: Option<u8>,
    tx_power_dbm: Option<i8>,
    periodic_interval: u16,
    payload_len: usize,
    raw_payload_hex: String,
}

pub fn export_file(input: &Path, output: &Path, format: ExportFormat) -> Result<u64> {
    let bytes = fs::read(input).with_context(|| format!("failed to read {}", input.display()))?;
    verify_bytes(&bytes, false)?;
    let clocks = collect_clock_mappings(&bytes)?;
    let file =
        File::create(output).with_context(|| format!("failed to create {}", output.display()))?;
    let mut count = 0u64;

    match format {
        ExportFormat::Jsonl => {
            let mut writer = BufWriter::new(file);
            walk_records(&bytes, |record| {
                if let DecodedRecord::BleObservation(observation) = record {
                    serde_json::to_writer(&mut writer, &row(observation, &clocks))?;
                    writer.write_all(b"\n")?;
                    count += 1;
                }
                Ok(())
            })?;
            writer.flush()?;
        }
        ExportFormat::Csv => {
            let mut writer = csv::Writer::from_writer(BufWriter::new(file));
            walk_records(&bytes, |record| {
                if let DecodedRecord::BleObservation(observation) = record {
                    writer.serialize(row(observation, &clocks))?;
                    count += 1;
                }
                Ok(())
            })?;
            writer.flush()?;
        }
    }
    Ok(count)
}

type ClockMap = HashMap<u64, Vec<(u64, i64)>>;

fn collect_clock_mappings(bytes: &[u8]) -> Result<ClockMap> {
    let mut clocks: ClockMap = HashMap::new();
    walk_records(bytes, |record| {
        match record {
            DecodedRecord::SessionStart(start) if start.wall_clock_unix_ms != UNIX_MS_UNKNOWN => {
                clocks
                    .entry(start.session_id)
                    .or_default()
                    .push((start.clock_monotonic_us, start.wall_clock_unix_ms));
            }
            DecodedRecord::ClockSync(sync) => {
                clocks
                    .entry(sync.session_id)
                    .or_default()
                    .push((sync.monotonic_us, sync.unix_ms));
            }
            _ => {}
        }
        Ok(())
    })?;
    for values in clocks.values_mut() {
        values.sort_unstable_by_key(|mapping| mapping.0);
    }
    Ok(clocks)
}

fn unix_time(clocks: &ClockMap, session_id: u64, monotonic_us: u64) -> Option<i64> {
    let mappings = clocks.get(&session_id)?;
    let &(base_monotonic, base_unix) = mappings
        .iter()
        .rev()
        .find(|(mono, _)| *mono <= monotonic_us)
        .or_else(|| mappings.first())?;
    let delta_us = i128::from(monotonic_us) - i128::from(base_monotonic);
    i64::try_from(i128::from(base_unix) + delta_us / 1000).ok()
}

fn row(observation: blelog_format::BleObservation<'_>, clocks: &ClockMap) -> ObservationRow {
    ObservationRow {
        session_id: format!("{:016x}", observation.session_id),
        sequence: observation.sequence,
        monotonic_timestamp_us: observation.monotonic_us,
        unix_timestamp_ms: unix_time(clocks, observation.session_id, observation.monotonic_us),
        address: format_address(observation.address),
        address_type: match observation.address_type {
            0 => Some("public"),
            1 => Some("random"),
            2 => Some("public_identity"),
            3 => Some("random_identity"),
            _ => None,
        },
        rssi_dbm: observation.rssi_dbm,
        report_kind: match observation.report_kind {
            ReportKind::Advertisement => "advertisement",
            ReportKind::ScanResponse => "scan_response",
            ReportKind::Combined => "combined",
            ReportKind::Unknown => "unknown",
        },
        data_status: optional_u8(observation.data_status, DATA_STATUS_UNKNOWN),
        legacy_event_type: optional_u8(observation.legacy_event_type, LEGACY_EVENT_UNKNOWN),
        extended_properties: (observation.extended_properties != EXTENDED_PROPERTIES_UNKNOWN)
            .then_some(observation.extended_properties),
        primary_phy: optional_u8(observation.primary_phy, PHY_UNKNOWN),
        secondary_phy: optional_u8(observation.secondary_phy, PHY_UNKNOWN),
        advertising_sid: optional_u8(observation.advertising_sid, SID_UNKNOWN),
        tx_power_dbm: (observation.tx_power_dbm != TX_POWER_UNKNOWN)
            .then_some(observation.tx_power_dbm),
        periodic_interval: observation.periodic_interval,
        payload_len: observation.payload.len(),
        raw_payload_hex: lower_hex(observation.payload),
    }
}

fn walk_records<'a>(
    mut bytes: &'a [u8],
    mut visit: impl FnMut(DecodedRecord<'a>) -> Result<()>,
) -> Result<()> {
    while !bytes.is_empty() {
        let decoded = decode_record(bytes)?;
        visit(decoded.record)?;
        bytes = &bytes[decoded.total_len..];
    }
    Ok(())
}

fn optional_u8(value: u8, sentinel: u8) -> Option<u8> {
    (value != sentinel).then_some(value)
}

fn format_address(bytes: [u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5]
    )
}

fn lower_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}
