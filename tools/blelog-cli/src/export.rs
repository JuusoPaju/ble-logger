use std::{
    collections::HashMap,
    fs,
    io::{BufWriter, Write},
    path::Path,
};

use anyhow::{Context, Result};
use blelog_format::capture::{CaptureRecord, Radios, WIFI_FCS_INCLUDED, WIFI_METADATA_ONLY};
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
    Pcapng,
}

#[derive(Debug, Serialize)]
struct ObservationRow {
    radio: &'static str,
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
    export_filtered(input, output, format, Radios::Both)
}

pub fn export_filtered(
    input: &Path,
    output: &Path,
    format: ExportFormat,
    radios: Radios,
) -> Result<u64> {
    let bytes = fs::read(input).with_context(|| format!("failed to read {}", input.display()))?;
    let scan = verify_bytes(&bytes, false)?;
    let wifi = crate::verify::analyze_wifi(&bytes)?;
    if wifi.incomplete != 0 {
        eprintln!(
            "{} incomplete Wi-Fi frame(s) retained in the archive but excluded from export",
            wifi.incomplete
        );
    }
    if wifi.unknown_records != 0 {
        eprintln!("{} unknown record(s) skipped", wifi.unknown_records);
    }
    if matches!(format, ExportFormat::Csv)
        && radios == Radios::Both
        && scan.observation_count > 0
        && wifi.frames > 0
    {
        anyhow::bail!(
            "mixed captures need --radio ble or --radio wifi for CSV; JSONL supports both"
        );
    }
    if matches!(format, ExportFormat::Pcapng) && radios == Radios::Ble {
        anyhow::bail!("PCAPNG supports Wi-Fi frames; use JSONL/CSV for BLE reports");
    }
    let clocks = collect_clock_mappings(&bytes)?;
    if input == output || (output.exists() && fs::canonicalize(input)? == fs::canonicalize(output)?)
    {
        anyhow::bail!("export cannot replace the canonical input");
    }
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .with_context(|| {
            format!(
                "failed to create {} (output must not exist)",
                output.display()
            )
        })?;
    let mut count = 0u64;

    match format {
        ExportFormat::Jsonl => {
            let mut writer = BufWriter::new(file);
            let mut assembler = crate::wifi::Assembler::default();
            walk_records(&bytes, |record| {
                if let DecodedRecord::BleObservation(observation) = record {
                    if radios != Radios::Wifi {
                        serde_json::to_writer(&mut writer, &row(observation, &clocks))?;
                        writer.write_all(b"\n")?;
                        count += 1;
                    }
                }
                if let DecodedRecord::Capture(CaptureRecord::Wifi(fragment)) = record {
                    if let Some(frame) = assembler.push(fragment)? {
                        if radios != Radios::Ble {
                            serde_json::to_writer(&mut writer, &wifi_row(&frame, &clocks))?;
                            writer.write_all(b"\n")?;
                            count += 1;
                        }
                    }
                }
                if matches!(
                    record,
                    DecodedRecord::SessionStart(_) | DecodedRecord::SessionEnd(_)
                ) {
                    assembler.boundary();
                }
                Ok(())
            })?;
            writer.flush()?;
        }
        ExportFormat::Csv => {
            let mut writer = csv::Writer::from_writer(BufWriter::new(file));
            let mut assembler = crate::wifi::Assembler::default();
            walk_records(&bytes, |record| {
                if let DecodedRecord::BleObservation(observation) = record {
                    if radios != Radios::Wifi {
                        writer.serialize(row(observation, &clocks))?;
                        count += 1;
                    }
                }
                if let DecodedRecord::Capture(CaptureRecord::Wifi(fragment)) = record {
                    if let Some(frame) = assembler.push(fragment)? {
                        if radios != Radios::Ble {
                            writer.serialize(wifi_row(&frame, &clocks))?;
                            count += 1;
                        }
                    }
                }
                if matches!(
                    record,
                    DecodedRecord::SessionStart(_) | DecodedRecord::SessionEnd(_)
                ) {
                    assembler.boundary();
                }
                Ok(())
            })?;
            writer.flush()?;
        }
        ExportFormat::Pcapng => {
            let mut writer = BufWriter::new(file);
            pcap_header(&mut writer)?;
            let mut assembler = crate::wifi::Assembler::default();
            walk_records(&bytes, |record| {
                if let DecodedRecord::Capture(CaptureRecord::Wifi(fragment)) = record {
                    if let Some(frame) = assembler.push(fragment)? {
                        if frame.meta.flags & WIFI_METADATA_ONLY == 0 && !frame.bytes.is_empty() {
                            pcap_packet(&mut writer, &frame, &clocks)?;
                            count += 1;
                        }
                    }
                }
                if matches!(
                    record,
                    DecodedRecord::SessionStart(_) | DecodedRecord::SessionEnd(_)
                ) {
                    assembler.boundary();
                }
                Ok(())
            })?;
            writer.flush()?;
            eprintln!("PCAPNG contains Wi-Fi byte frames only; unsynchronized sessions use monotonic microseconds as a synthetic epoch (see packet comments)");
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
        radio: "ble",
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

#[derive(Serialize)]
struct WifiRow {
    radio: &'static str,
    session_id: String,
    sequence: u32,
    monotonic_timestamp_us: u64,
    unix_timestamp_ms: Option<i64>,
    window_id: u32,
    hardware_timestamp_us: u32,
    channel: u8,
    secondary_channel: u8,
    rssi_dbm: i8,
    driver_packet_type: u8,
    frame_type: Option<u8>,
    frame_subtype: Option<u8>,
    receiver: Option<String>,
    transmitter: Option<String>,
    source: Option<String>,
    destination: Option<String>,
    bssid: Option<String>,
    ssid: Option<String>,
    ssid_hex: Option<String>,
    protected: Option<bool>,
    retry: Option<bool>,
    rate: Option<u8>,
    sig_mode: u8,
    mcs: Option<u8>,
    bandwidth: u8,
    aggregation: u8,
    rx_state: u8,
    original_len: u16,
    captured_len: u16,
    metadata_only: bool,
    fcs_included: bool,
    raw_payload_hex: String,
}
fn wifi_row(frame: &crate::wifi::WifiFrame, clocks: &ClockMap) -> WifiRow {
    let m = frame.meta;
    let b = &frame.bytes;
    let fc = b.get(..2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let kind = fc.map(|f| ((f >> 2) & 3) as u8);
    let subtype = fc.map(|f| ((f >> 4) & 15) as u8);
    let addr = |offset: usize| {
        b.get(offset..offset + 6)
            .map(|v| format_address(v.try_into().unwrap()))
    };
    let receiver = addr(4);
    let transmitter = if kind == Some(1) && !matches!(subtype, Some(8 | 9 | 10 | 11 | 14 | 15)) {
        None
    } else {
        addr(10)
    };
    let (source, destination, bssid) = match (kind, fc.map(|f| (f >> 8) & 3)) {
        (Some(0), _) | (Some(2), Some(0)) => (addr(10), addr(4), addr(16)),
        (Some(2), Some(1)) => (addr(10), addr(16), addr(4)),
        (Some(2), Some(2)) => (addr(16), addr(4), addr(10)),
        (Some(2), Some(3)) => (addr(24), addr(16), None),
        _ => (None, None, None),
    };
    let ssid_bytes = if kind == Some(0) {
        let offset = match subtype {
            Some(8 | 5) => Some(36),
            Some(4) => Some(24),
            _ => None,
        };
        offset.and_then(|start| {
            let end = b.len().saturating_sub(if m.flags & WIFI_FCS_INCLUDED != 0 {
                4
            } else {
                0
            });
            let mut pos = start;
            while pos + 2 <= end {
                let tag = b[pos];
                let len = b[pos + 1] as usize;
                pos += 2;
                if pos + len > end {
                    return None;
                }
                if tag == 0 {
                    return Some(&b[pos..pos + len]);
                }
                pos += len;
            }
            None
        })
    } else {
        None
    };
    WifiRow {
        radio: "wifi",
        session_id: format!("{:016x}", m.session_id),
        sequence: m.sequence,
        monotonic_timestamp_us: m.monotonic_us,
        unix_timestamp_ms: unix_time(clocks, m.session_id, m.monotonic_us),
        window_id: m.window_id,
        hardware_timestamp_us: m.hardware_timestamp_us,
        channel: m.channel,
        secondary_channel: m.secondary_channel,
        rssi_dbm: m.rssi_dbm,
        driver_packet_type: m.packet_type,
        frame_type: kind,
        frame_subtype: subtype,
        receiver,
        transmitter,
        source,
        destination,
        bssid,
        ssid: ssid_bytes
            .and_then(|s| std::str::from_utf8(s).ok())
            .map(str::to_owned),
        ssid_hex: ssid_bytes.map(lower_hex),
        protected: fc.map(|f| f & 0x4000 != 0),
        retry: fc.map(|f| f & 0x0800 != 0),
        rate: (m.sig_mode == 0).then_some(m.rate),
        sig_mode: m.sig_mode,
        mcs: (m.sig_mode == 1).then_some(m.mcs),
        bandwidth: m.bandwidth,
        aggregation: m.aggregation,
        rx_state: m.rx_state,
        original_len: m.original_len,
        captured_len: m.captured_len,
        metadata_only: m.flags & WIFI_METADATA_ONLY != 0,
        fcs_included: m.flags & WIFI_FCS_INCLUDED != 0,
        raw_payload_hex: lower_hex(b),
    }
}

fn pcap_block(writer: &mut impl Write, kind: u32, mut body: Vec<u8>) -> Result<()> {
    while body.len() % 4 != 0 {
        body.push(0);
    }
    let len = (body.len() + 12) as u32;
    writer.write_all(&kind.to_le_bytes())?;
    writer.write_all(&len.to_le_bytes())?;
    writer.write_all(&body)?;
    writer.write_all(&len.to_le_bytes())?;
    Ok(())
}
fn pcap_header(writer: &mut impl Write) -> Result<()> {
    let mut section = Vec::new();
    section.extend_from_slice(&0x1a2b3c4du32.to_le_bytes());
    section.extend_from_slice(&1u16.to_le_bytes());
    section.extend_from_slice(&0u16.to_le_bytes());
    section.extend_from_slice(&(-1i64).to_le_bytes());
    pcap_block(writer, 0x0a0d0d0a, section)?;
    let mut interface = Vec::new();
    interface.extend_from_slice(&127u16.to_le_bytes());
    interface.extend_from_slice(&0u16.to_le_bytes());
    interface.extend_from_slice(&65535u32.to_le_bytes());
    pcap_block(writer, 1, interface)
}
fn pcap_packet(
    writer: &mut impl Write,
    frame: &crate::wifi::WifiFrame,
    clocks: &ClockMap,
) -> Result<()> {
    let m = frame.meta;
    let wall = unix_time(clocks, m.session_id, m.monotonic_us);
    let timestamp = wall
        .and_then(|ms| u64::try_from(ms).ok())
        .and_then(|ms| ms.checked_mul(1000))
        .unwrap_or(m.monotonic_us);
    // Radiotap FLAGS, CHANNEL and DBM_ANTSIGNAL. Padding aligns CHANNEL to u16.
    let mut radio = vec![0, 0, 15, 0, 0x2a, 0, 0, 0];
    let mut flags = if m.flags & WIFI_FCS_INCLUDED != 0 {
        0x10
    } else {
        0
    };
    if flags != 0 && frame.bytes.len() >= 4 && m.captured_len == m.original_len {
        let split = frame.bytes.len() - 4;
        if blelog_format::crc::crc32(&frame.bytes[..split])
            != u32::from_le_bytes(frame.bytes[split..].try_into().unwrap())
        {
            flags |= 0x40;
        }
    }
    radio.extend_from_slice(&[flags, 0]);
    radio.extend_from_slice(&(2407 + 5 * u16::from(m.channel)).to_le_bytes());
    // Only claim the known 2 GHz band, not an inferred CCK/OFDM modulation.
    radio.extend_from_slice(&0x0080u16.to_le_bytes());
    radio.push(m.rssi_dbm as u8);
    let mut body = Vec::new();
    body.extend_from_slice(&0u32.to_le_bytes());
    body.extend_from_slice(&((timestamp >> 32) as u32).to_le_bytes());
    body.extend_from_slice(&(timestamp as u32).to_le_bytes());
    body.extend_from_slice(&((radio.len() + frame.bytes.len()) as u32).to_le_bytes());
    body.extend_from_slice(&((radio.len() + m.original_len as usize) as u32).to_le_bytes());
    body.extend_from_slice(&radio);
    body.extend_from_slice(&frame.bytes);
    while body.len() % 4 != 0 {
        body.push(0);
    }
    let comment = format!(
        "session={:016x} monotonic_us={} clock={} driver_rx_state={} aggregation={}",
        m.session_id,
        m.monotonic_us,
        if wall.is_some() {
            "unix"
        } else {
            "unknown; synthetic epoch"
        },
        m.rx_state,
        m.aggregation
    );
    body.extend_from_slice(&1u16.to_le_bytes());
    body.extend_from_slice(&(comment.len() as u16).to_le_bytes());
    body.extend_from_slice(comment.as_bytes());
    while body.len() % 4 != 0 {
        body.push(0);
    }
    body.extend_from_slice(&[0; 4]);
    pcap_block(writer, 6, body)
}
