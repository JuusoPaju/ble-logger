use core::fmt;

use crate::crc::crc32;

pub const MAGIC: [u8; 4] = *b"BLER";
pub const FORMAT_VERSION: u8 = 1;
pub const ENVELOPE_LEN: usize = 12;
pub const CRC_LEN: usize = 4;
pub const MAX_RECORD_LEN: usize = 1024;
pub const MAX_OBSERVATION_PAYLOAD: usize = 255;
pub const MAX_FIRMWARE_VERSION_LEN: usize = 63;
pub const UNIX_MS_UNKNOWN: i64 = -1;
pub const PHY_UNKNOWN: u8 = 0xFF;
pub const SID_UNKNOWN: u8 = 0xFF;
pub const TX_POWER_UNKNOWN: i8 = 0x7F;
pub const DATA_STATUS_UNKNOWN: u8 = 0xFF;
pub const LEGACY_EVENT_UNKNOWN: u8 = 0xFF;
pub const EXTENDED_PROPERTIES_UNKNOWN: u16 = 0xFFFF;

pub const TYPE_SESSION_START: u8 = 0x01;
pub const TYPE_BLE_OBSERVATION: u8 = 0x02;
pub const TYPE_SESSION_END: u8 = 0x03;
pub const TYPE_CLOCK_SYNC: u8 = 0x04;

pub const SESSION_START_HEADER_LEN: usize = 56;
pub const OBSERVATION_HEADER_LEN: usize = 54;
pub const SESSION_END_HEADER_LEN: usize = 68;
pub const CLOCK_SYNC_HEADER_LEN: usize = 36;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanMode {
    Passive = 0,
    Active = 1,
}

impl TryFrom<u8> for ScanMode {
    type Error = RecordError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Passive),
            1 => Ok(Self::Active),
            _ => Err(RecordError::InvalidField("scan_mode")),
        }
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportKind {
    Advertisement = 0,
    ScanResponse = 1,
    Combined = 2,
    Unknown = 0xFF,
}

impl From<u8> for ReportKind {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::Advertisement,
            1 => Self::ScanResponse,
            2 => Self::Combined,
            _ => Self::Unknown,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionStart<'a> {
    pub session_id: u64,
    pub boot_id: u32,
    pub start_monotonic_us: u64,
    pub wall_clock_unix_ms: i64,
    pub clock_monotonic_us: u64,
    pub scan_mode: ScanMode,
    pub scan_interval_ms: u16,
    pub scan_window_ms: u16,
    pub extended_adv_enabled: bool,
    pub firmware_version: &'a [u8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BleObservation<'a> {
    pub session_id: u64,
    pub sequence: u32,
    pub monotonic_us: u64,
    /// Canonical display order (the first byte prints first).
    pub address: [u8; 6],
    pub address_type: u8,
    pub rssi_dbm: i8,
    pub report_kind: ReportKind,
    pub data_status: u8,
    pub primary_phy: u8,
    pub secondary_phy: u8,
    pub advertising_sid: u8,
    pub tx_power_dbm: i8,
    pub legacy_event_type: u8,
    pub extended_properties: u16,
    pub periodic_interval: u16,
    pub payload: &'a [u8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionEnd {
    pub session_id: u64,
    pub stop_monotonic_us: u64,
    pub total_observations: u64,
    pub stored_observations: u64,
    pub dropped_queue_full: u64,
    pub dropped_storage_full: u64,
    pub flash_write_errors: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockSync {
    pub session_id: u64,
    pub unix_ms: i64,
    pub monotonic_us: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Record<'a> {
    Capture(crate::capture::CaptureRecord<'a>),
    SessionStart(SessionStart<'a>),
    BleObservation(BleObservation<'a>),
    SessionEnd(SessionEnd),
    ClockSync(ClockSync),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodedRecord<'a> {
    Capture(crate::capture::CaptureRecord<'a>),
    SessionStart(SessionStart<'a>),
    BleObservation(BleObservation<'a>),
    SessionEnd(SessionEnd),
    ClockSync(ClockSync),
    Unknown { record_type: u8, bytes: &'a [u8] },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decoded<'a> {
    pub record: DecodedRecord<'a>,
    pub total_len: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordError {
    BufferTooSmall,
    PayloadTooLong,
    FirmwareVersionTooLong,
    InvalidMagic,
    UnsupportedVersion(u8),
    InvalidHeaderLength,
    InvalidTotalLength,
    RecordTooLong,
    Truncated,
    NonZeroPadding,
    CrcMismatch { expected: u32, actual: u32 },
    InvalidField(&'static str),
}

impl fmt::Display for RecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

#[cfg(feature = "std")]
impl std::error::Error for RecordError {}

fn aligned_len(unpadded: usize) -> Result<usize, RecordError> {
    unpadded
        .checked_add(3)
        .map(|n| n & !3)
        .filter(|&n| n <= MAX_RECORD_LEN)
        .ok_or(RecordError::RecordTooLong)
}

fn write_u16(out: &mut [u8], offset: usize, value: u16) {
    out[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn write_u32(out: &mut [u8], offset: usize, value: u32) {
    out[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(out: &mut [u8], offset: usize, value: u64) {
    out[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn write_i64(out: &mut [u8], offset: usize, value: i64) {
    out[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn read_u16(input: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([input[offset], input[offset + 1]])
}

fn read_u32(input: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(input[offset..offset + 4].try_into().unwrap())
}

fn read_u64(input: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(input[offset..offset + 8].try_into().unwrap())
}

fn read_i64(input: &[u8], offset: usize) -> i64 {
    i64::from_le_bytes(input[offset..offset + 8].try_into().unwrap())
}

pub fn encoded_len(record: &Record<'_>) -> Result<usize, RecordError> {
    let (header, payload) = match record {
        Record::Capture(v) => (14, v.body_len()?),
        Record::SessionStart(v) => {
            if v.firmware_version.len() > MAX_FIRMWARE_VERSION_LEN {
                return Err(RecordError::FirmwareVersionTooLong);
            }
            (SESSION_START_HEADER_LEN, v.firmware_version.len())
        }
        Record::BleObservation(v) => {
            if v.payload.len() > MAX_OBSERVATION_PAYLOAD {
                return Err(RecordError::PayloadTooLong);
            }
            (OBSERVATION_HEADER_LEN, v.payload.len())
        }
        Record::SessionEnd(_) => (SESSION_END_HEADER_LEN, 0),
        Record::ClockSync(_) => (CLOCK_SYNC_HEADER_LEN, 0),
    };
    aligned_len(header + payload + CRC_LEN)
}

/// Encode one fully committed record image. Firmware must program `out[4..len]`
/// first, then program `out[..4]` as the final commit marker.
pub fn encode_record(record: &Record<'_>, out: &mut [u8]) -> Result<usize, RecordError> {
    let len = encoded_len(record)?;
    if out.len() < len {
        return Err(RecordError::BufferTooSmall);
    }
    out[..len].fill(0);
    out[..4].copy_from_slice(&MAGIC);
    out[4] = FORMAT_VERSION;
    write_u32(out, 8, len as u32);

    let (record_type, header_len, payload): (u8, usize, &[u8]) = match record {
        Record::Capture(v) => {
            let body_len = v.body_len()?;
            write_u16(out, 12, body_len as u16);
            v.encode_body(&mut out[14..14 + body_len])?;
            (v.record_type(), 14, &[])
        }
        Record::SessionStart(v) => {
            write_u64(out, 12, v.session_id);
            write_u32(out, 20, v.boot_id);
            write_u64(out, 24, v.start_monotonic_us);
            write_i64(out, 32, v.wall_clock_unix_ms);
            write_u64(out, 40, v.clock_monotonic_us);
            out[48] = v.scan_mode as u8;
            out[49] = u8::from(v.extended_adv_enabled);
            write_u16(out, 50, v.scan_interval_ms);
            write_u16(out, 52, v.scan_window_ms);
            out[54] = v.firmware_version.len() as u8;
            (
                TYPE_SESSION_START,
                SESSION_START_HEADER_LEN,
                v.firmware_version,
            )
        }
        Record::BleObservation(v) => {
            write_u64(out, 12, v.session_id);
            write_u32(out, 20, v.sequence);
            write_u64(out, 24, v.monotonic_us);
            out[32..38].copy_from_slice(&v.address);
            out[38] = v.address_type;
            out[39] = v.rssi_dbm as u8;
            out[40] = v.report_kind as u8;
            out[41] = v.data_status;
            out[42] = v.primary_phy;
            out[43] = v.secondary_phy;
            out[44] = v.advertising_sid;
            out[45] = v.tx_power_dbm as u8;
            out[46] = v.legacy_event_type;
            write_u16(out, 48, v.extended_properties);
            write_u16(out, 50, v.periodic_interval);
            write_u16(out, 52, v.payload.len() as u16);
            (TYPE_BLE_OBSERVATION, OBSERVATION_HEADER_LEN, v.payload)
        }
        Record::SessionEnd(v) => {
            write_u64(out, 12, v.session_id);
            write_u64(out, 20, v.stop_monotonic_us);
            write_u64(out, 28, v.total_observations);
            write_u64(out, 36, v.stored_observations);
            write_u64(out, 44, v.dropped_queue_full);
            write_u64(out, 52, v.dropped_storage_full);
            write_u64(out, 60, v.flash_write_errors);
            (TYPE_SESSION_END, SESSION_END_HEADER_LEN, &[])
        }
        Record::ClockSync(v) => {
            write_u64(out, 12, v.session_id);
            write_i64(out, 20, v.unix_ms);
            write_u64(out, 28, v.monotonic_us);
            (TYPE_CLOCK_SYNC, CLOCK_SYNC_HEADER_LEN, &[])
        }
    };
    out[5] = record_type;
    write_u16(out, 6, header_len as u16);
    out[header_len..header_len + payload.len()].copy_from_slice(payload);
    let crc_offset = len - CRC_LEN;
    write_u32(out, crc_offset, crc32(&out[..crc_offset]));
    Ok(len)
}

pub fn decode_record(input: &[u8]) -> Result<Decoded<'_>, RecordError> {
    if input.len() < ENVELOPE_LEN {
        return Err(RecordError::Truncated);
    }
    if input[..4] != MAGIC {
        return Err(RecordError::InvalidMagic);
    }
    if input[4] != FORMAT_VERSION {
        return Err(RecordError::UnsupportedVersion(input[4]));
    }
    let record_type = input[5];
    let header_len = usize::from(read_u16(input, 6));
    let total_len = read_u32(input, 8) as usize;
    if header_len < ENVELOPE_LEN || header_len > total_len.saturating_sub(CRC_LEN) {
        return Err(RecordError::InvalidHeaderLength);
    }
    if total_len < ENVELOPE_LEN + CRC_LEN || total_len & 3 != 0 {
        return Err(RecordError::InvalidTotalLength);
    }
    if total_len > MAX_RECORD_LEN {
        return Err(RecordError::RecordTooLong);
    }
    if input.len() < total_len {
        return Err(RecordError::Truncated);
    }
    let bytes = &input[..total_len];
    let crc_offset = total_len - CRC_LEN;
    let expected = read_u32(bytes, crc_offset);
    let actual = crc32(&bytes[..crc_offset]);
    if expected != actual {
        return Err(RecordError::CrcMismatch { expected, actual });
    }

    let record = match record_type {
        5..=8 => {
            if header_len != 14 {
                return Err(RecordError::InvalidHeaderLength);
            }
            let body_len = usize::from(read_u16(bytes, 12));
            if header_len + body_len > crc_offset {
                return Err(RecordError::Truncated);
            }
            validate_padding(bytes, header_len + body_len, crc_offset)?;
            DecodedRecord::Capture(crate::capture::decode_body(
                record_type,
                &bytes[14..14 + body_len],
            )?)
        }
        TYPE_SESSION_START => {
            if header_len != SESSION_START_HEADER_LEN {
                return Err(RecordError::InvalidHeaderLength);
            }
            let firmware_len = usize::from(bytes[54]);
            if firmware_len > MAX_FIRMWARE_VERSION_LEN || header_len + firmware_len > crc_offset {
                return Err(RecordError::InvalidField("firmware_version_len"));
            }
            validate_padding(bytes, header_len + firmware_len, crc_offset)?;
            DecodedRecord::SessionStart(SessionStart {
                session_id: read_u64(bytes, 12),
                boot_id: read_u32(bytes, 20),
                start_monotonic_us: read_u64(bytes, 24),
                wall_clock_unix_ms: read_i64(bytes, 32),
                clock_monotonic_us: read_u64(bytes, 40),
                scan_mode: ScanMode::try_from(bytes[48])?,
                extended_adv_enabled: match bytes[49] {
                    0 => false,
                    1 => true,
                    _ => return Err(RecordError::InvalidField("extended_adv_enabled")),
                },
                scan_interval_ms: read_u16(bytes, 50),
                scan_window_ms: read_u16(bytes, 52),
                firmware_version: &bytes[header_len..header_len + firmware_len],
            })
        }
        TYPE_BLE_OBSERVATION => {
            if header_len != OBSERVATION_HEADER_LEN {
                return Err(RecordError::InvalidHeaderLength);
            }
            let payload_len = usize::from(read_u16(bytes, 52));
            if payload_len > MAX_OBSERVATION_PAYLOAD || header_len + payload_len > crc_offset {
                return Err(RecordError::InvalidField("payload_len"));
            }
            validate_padding(bytes, header_len + payload_len, crc_offset)?;
            DecodedRecord::BleObservation(BleObservation {
                session_id: read_u64(bytes, 12),
                sequence: read_u32(bytes, 20),
                monotonic_us: read_u64(bytes, 24),
                address: bytes[32..38].try_into().unwrap(),
                address_type: bytes[38],
                rssi_dbm: bytes[39] as i8,
                report_kind: bytes[40].into(),
                data_status: bytes[41],
                primary_phy: bytes[42],
                secondary_phy: bytes[43],
                advertising_sid: bytes[44],
                tx_power_dbm: bytes[45] as i8,
                legacy_event_type: bytes[46],
                extended_properties: read_u16(bytes, 48),
                periodic_interval: read_u16(bytes, 50),
                payload: &bytes[header_len..header_len + payload_len],
            })
        }
        TYPE_SESSION_END => {
            if header_len != SESSION_END_HEADER_LEN || header_len != crc_offset {
                return Err(RecordError::InvalidHeaderLength);
            }
            DecodedRecord::SessionEnd(SessionEnd {
                session_id: read_u64(bytes, 12),
                stop_monotonic_us: read_u64(bytes, 20),
                total_observations: read_u64(bytes, 28),
                stored_observations: read_u64(bytes, 36),
                dropped_queue_full: read_u64(bytes, 44),
                dropped_storage_full: read_u64(bytes, 52),
                flash_write_errors: read_u64(bytes, 60),
            })
        }
        TYPE_CLOCK_SYNC => {
            if header_len != CLOCK_SYNC_HEADER_LEN || header_len != crc_offset {
                return Err(RecordError::InvalidHeaderLength);
            }
            DecodedRecord::ClockSync(ClockSync {
                session_id: read_u64(bytes, 12),
                unix_ms: read_i64(bytes, 20),
                monotonic_us: read_u64(bytes, 28),
            })
        }
        _ => DecodedRecord::Unknown { record_type, bytes },
    };
    Ok(Decoded { record, total_len })
}

fn validate_padding(bytes: &[u8], data_end: usize, crc_offset: usize) -> Result<(), RecordError> {
    if bytes[data_end..crc_offset].iter().any(|&b| b != 0) {
        return Err(RecordError::NonZeroPadding);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamScan {
    pub valid_bytes: usize,
    pub record_count: u64,
    pub observation_count: u64,
    pub tail: StreamTail,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamTail {
    ExactEnd,
    Erased,
    Corrupt(RecordError),
    UnsupportedVersion(u8),
}

/// Validate a partition image or host file. An erased tail is clean only when
/// every remaining byte is `0xFF`; a partially programmed tail is corrupt.
pub fn scan_stream(input: &[u8]) -> StreamScan {
    let mut offset = 0;
    let mut record_count = 0;
    let mut observation_count = 0;
    while offset < input.len() {
        if input[offset] == 0xFF {
            let tail = if input[offset..].iter().all(|&b| b == 0xFF) {
                StreamTail::Erased
            } else {
                StreamTail::Corrupt(RecordError::InvalidMagic)
            };
            return StreamScan {
                valid_bytes: offset,
                record_count,
                observation_count,
                tail,
            };
        }
        match decode_record(&input[offset..]) {
            Ok(decoded) => {
                record_count += 1;
                if matches!(decoded.record, DecodedRecord::BleObservation(_)) {
                    observation_count += 1;
                }
                offset += decoded.total_len;
            }
            Err(RecordError::UnsupportedVersion(version)) => {
                return StreamScan {
                    valid_bytes: offset,
                    record_count,
                    observation_count,
                    tail: StreamTail::UnsupportedVersion(version),
                };
            }
            Err(error) => {
                return StreamScan {
                    valid_bytes: offset,
                    record_count,
                    observation_count,
                    tail: StreamTail::Corrupt(error),
                };
            }
        }
    }
    StreamScan {
        valid_bytes: offset,
        record_count,
        observation_count,
        tail: StreamTail::ExactEnd,
    }
}
