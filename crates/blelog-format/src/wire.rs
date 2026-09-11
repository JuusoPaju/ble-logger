//! Command payload schemas. Response payloads are prefixed by transport `Status`.

use core::{fmt, str};

use crate::record::ScanMode;

pub const MAX_DUMP_CHUNK: u16 = 4096;
pub const MAX_SCAN_MS: u16 = 10_240;
pub const INFO_FIXED_LEN: usize = 46;
pub const STATS_LEN: usize = 52;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoggerState {
    Booting = 0,
    Recovering = 1,
    Ready = 2,
    Capturing = 3,
    Stopping = 4,
    Dumping = 5,
    Erasing = 6,
    Full = 7,
    CorruptTail = 8,
    Error = 9,
}

impl TryFrom<u8> for LoggerState {
    type Error = WireError;

    fn try_from(value: u8) -> Result<Self, WireError> {
        Ok(match value {
            0 => Self::Booting,
            1 => Self::Recovering,
            2 => Self::Ready,
            3 => Self::Capturing,
            4 => Self::Stopping,
            5 => Self::Dumping,
            6 => Self::Erasing,
            7 => Self::Full,
            8 => Self::CorruptTail,
            9 => Self::Error,
            _ => return Err(WireError::InvalidField("logger_state")),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StartCapture {
    pub scan_mode: ScanMode,
    pub interval_ms: u16,
    pub window_ms: u16,
}

impl StartCapture {
    pub const LEN: usize = 5;

    pub fn encode(self, out: &mut [u8]) -> Result<usize, WireError> {
        validate_scan(self)?;
        need(out, Self::LEN)?;
        out[0] = self.scan_mode as u8;
        out[1..3].copy_from_slice(&self.interval_ms.to_le_bytes());
        out[3..5].copy_from_slice(&self.window_ms.to_le_bytes());
        Ok(Self::LEN)
    }

    pub fn decode(input: &[u8]) -> Result<Self, WireError> {
        exact(input, Self::LEN)?;
        let value = Self {
            scan_mode: ScanMode::try_from(input[0])
                .map_err(|_| WireError::InvalidField("scan_mode"))?,
            interval_ms: u16::from_le_bytes([input[1], input[2]]),
            window_ms: u16::from_le_bytes([input[3], input[4]]),
        };
        validate_scan(value)?;
        Ok(value)
    }
}

fn validate_scan(value: StartCapture) -> Result<(), WireError> {
    if value.interval_ms == 0
        || value.interval_ms > MAX_SCAN_MS
        || value.window_ms == 0
        || value.window_ms > value.interval_ms
    {
        return Err(WireError::InvalidField("scan_interval/window"));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Info<'a> {
    pub record_format: u8,
    pub state: LoggerState,
    pub scan_mode: ScanMode,
    pub extended_adv_enabled: bool,
    pub flash_size: u32,
    pub psram_size: u32,
    pub partition_size: u32,
    pub valid_bytes: u32,
    pub free_bytes: u32,
    pub record_count: u64,
    pub base_mac: [u8; 6],
    pub warning_flags: u16,
    pub scan_interval_ms: u16,
    pub scan_window_ms: u16,
    pub chip_model: &'a str,
    pub firmware_version: &'a str,
}

impl<'a> Info<'a> {
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, WireError> {
        let chip_len = u8::try_from(self.chip_model.len()).map_err(|_| WireError::TooLong)?;
        let fw_len = u8::try_from(self.firmware_version.len()).map_err(|_| WireError::TooLong)?;
        let total = INFO_FIXED_LEN + usize::from(chip_len) + usize::from(fw_len);
        need(out, total)?;
        out[0] = self.record_format;
        out[1] = self.state as u8;
        out[2] = self.scan_mode as u8;
        out[3] = u8::from(self.extended_adv_enabled);
        put_u32(out, 4, self.flash_size);
        put_u32(out, 8, self.psram_size);
        put_u32(out, 12, self.partition_size);
        put_u32(out, 16, self.valid_bytes);
        put_u32(out, 20, self.free_bytes);
        put_u64(out, 24, self.record_count);
        out[32..38].copy_from_slice(&self.base_mac);
        out[38] = chip_len;
        out[39] = fw_len;
        out[40..42].copy_from_slice(&self.warning_flags.to_le_bytes());
        out[42..44].copy_from_slice(&self.scan_interval_ms.to_le_bytes());
        out[44..46].copy_from_slice(&self.scan_window_ms.to_le_bytes());
        out[46..46 + usize::from(chip_len)].copy_from_slice(self.chip_model.as_bytes());
        out[46 + usize::from(chip_len)..total].copy_from_slice(self.firmware_version.as_bytes());
        Ok(total)
    }

    pub fn decode(input: &'a [u8]) -> Result<Self, WireError> {
        need(input, INFO_FIXED_LEN)?;
        let chip_len = usize::from(input[38]);
        let fw_len = usize::from(input[39]);
        exact(input, INFO_FIXED_LEN + chip_len + fw_len)?;
        let chip_end = INFO_FIXED_LEN + chip_len;
        Ok(Self {
            record_format: input[0],
            state: LoggerState::try_from(input[1])?,
            scan_mode: ScanMode::try_from(input[2])
                .map_err(|_| WireError::InvalidField("scan_mode"))?,
            extended_adv_enabled: match input[3] {
                0 => false,
                1 => true,
                _ => return Err(WireError::InvalidField("extended_adv_enabled")),
            },
            flash_size: get_u32(input, 4),
            psram_size: get_u32(input, 8),
            partition_size: get_u32(input, 12),
            valid_bytes: get_u32(input, 16),
            free_bytes: get_u32(input, 20),
            record_count: get_u64(input, 24),
            base_mac: input[32..38].try_into().unwrap(),
            warning_flags: u16::from_le_bytes([input[40], input[41]]),
            scan_interval_ms: u16::from_le_bytes([input[42], input[43]]),
            scan_window_ms: u16::from_le_bytes([input[44], input[45]]),
            chip_model: str::from_utf8(&input[INFO_FIXED_LEN..chip_end])
                .map_err(|_| WireError::InvalidUtf8)?,
            firmware_version: str::from_utf8(&input[chip_end..])
                .map_err(|_| WireError::InvalidUtf8)?,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub received_observations: u64,
    pub stored_observations: u64,
    pub dropped_queue_full: u64,
    pub dropped_storage_full: u64,
    pub flash_write_errors: u64,
    pub bytes_written: u64,
    pub queue_high_water: u32,
}

impl Stats {
    pub fn encode(self, out: &mut [u8]) -> Result<usize, WireError> {
        need(out, STATS_LEN)?;
        put_u64(out, 0, self.received_observations);
        put_u64(out, 8, self.stored_observations);
        put_u64(out, 16, self.dropped_queue_full);
        put_u64(out, 24, self.dropped_storage_full);
        put_u64(out, 32, self.flash_write_errors);
        put_u64(out, 40, self.bytes_written);
        put_u32(out, 48, self.queue_high_water);
        Ok(STATS_LEN)
    }

    pub fn decode(input: &[u8]) -> Result<Self, WireError> {
        exact(input, STATS_LEN)?;
        Ok(Self {
            received_observations: get_u64(input, 0),
            stored_observations: get_u64(input, 8),
            dropped_queue_full: get_u64(input, 16),
            dropped_storage_full: get_u64(input, 24),
            flash_write_errors: get_u64(input, 32),
            bytes_written: get_u64(input, 40),
            queue_high_water: get_u32(input, 48),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireError {
    Truncated,
    LengthMismatch,
    InvalidField(&'static str),
    InvalidUtf8,
    TooLong,
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

#[cfg(feature = "std")]
impl std::error::Error for WireError {}

fn need(input: &[u8], len: usize) -> Result<(), WireError> {
    if input.len() < len {
        Err(WireError::Truncated)
    } else {
        Ok(())
    }
}

fn exact(input: &[u8], len: usize) -> Result<(), WireError> {
    if input.len() == len {
        Ok(())
    } else {
        Err(WireError::LengthMismatch)
    }
}

fn put_u32(out: &mut [u8], offset: usize, value: u32) {
    out[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(out: &mut [u8], offset: usize, value: u64) {
    out[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn get_u32(input: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(input[offset..offset + 4].try_into().unwrap())
}

fn get_u64(input: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(input[offset..offset + 8].try_into().unwrap())
}

pub fn encode_dump_begin(
    transfer_id: u32,
    valid_bytes: u32,
    record_count: u64,
    out: &mut [u8],
) -> Result<usize, WireError> {
    need(out, 16)?;
    put_u32(out, 0, transfer_id);
    put_u32(out, 4, valid_bytes);
    put_u64(out, 8, record_count);
    Ok(16)
}

pub fn decode_dump_begin(input: &[u8]) -> Result<(u32, u32, u64), WireError> {
    exact(input, 16)?;
    Ok((get_u32(input, 0), get_u32(input, 4), get_u64(input, 8)))
}

pub fn encode_dump_read_request(
    transfer_id: u32,
    offset: u32,
    length: u16,
    out: &mut [u8],
) -> Result<usize, WireError> {
    if length == 0 || length > MAX_DUMP_CHUNK {
        return Err(WireError::InvalidField("dump_length"));
    }
    need(out, 10)?;
    put_u32(out, 0, transfer_id);
    put_u32(out, 4, offset);
    out[8..10].copy_from_slice(&length.to_le_bytes());
    Ok(10)
}

pub fn decode_dump_read_request(input: &[u8]) -> Result<(u32, u32, u16), WireError> {
    exact(input, 10)?;
    let length = u16::from_le_bytes([input[8], input[9]]);
    if length == 0 || length > MAX_DUMP_CHUNK {
        return Err(WireError::InvalidField("dump_length"));
    }
    Ok((get_u32(input, 0), get_u32(input, 4), length))
}

pub fn encode_dump_read_response(
    transfer_id: u32,
    offset: u32,
    bytes: &[u8],
    out: &mut [u8],
) -> Result<usize, WireError> {
    if bytes.len() > usize::from(MAX_DUMP_CHUNK) {
        return Err(WireError::TooLong);
    }
    let len = 10 + bytes.len();
    need(out, len)?;
    put_u32(out, 0, transfer_id);
    put_u32(out, 4, offset);
    out[8..10].copy_from_slice(&(bytes.len() as u16).to_le_bytes());
    out[10..len].copy_from_slice(bytes);
    Ok(len)
}

pub fn decode_dump_read_response(input: &[u8]) -> Result<(u32, u32, &[u8]), WireError> {
    need(input, 10)?;
    let len = usize::from(u16::from_le_bytes([input[8], input[9]]));
    if len > usize::from(MAX_DUMP_CHUNK) {
        return Err(WireError::TooLong);
    }
    exact(input, 10 + len)?;
    Ok((get_u32(input, 0), get_u32(input, 4), &input[10..]))
}

pub fn encode_u32(value: u32, out: &mut [u8]) -> Result<usize, WireError> {
    need(out, 4)?;
    put_u32(out, 0, value);
    Ok(4)
}

pub fn decode_u32(input: &[u8]) -> Result<u32, WireError> {
    exact(input, 4)?;
    Ok(get_u32(input, 0))
}

pub fn encode_i64(value: i64, out: &mut [u8]) -> Result<usize, WireError> {
    need(out, 8)?;
    out[..8].copy_from_slice(&value.to_le_bytes());
    Ok(8)
}

pub fn decode_i64(input: &[u8]) -> Result<i64, WireError> {
    exact(input, 8)?;
    Ok(i64::from_le_bytes(input.try_into().unwrap()))
}
