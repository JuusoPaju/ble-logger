use core::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FirmwareError {
    PartitionNotFound,
    FlashRead(i32),
    FlashWrite(i32),
    FlashErase(i32),
    Record(blelog_format::RecordError),
    CorruptTail,
    UnsupportedFormat(u8),
    Full,
    InvalidState,
    Bounds,
    Busy,
    QueueClosed,
    Usb(i32),
    Ble,
    Wifi(i32),
    Config(i32),
}

impl From<blelog_format::RecordError> for FirmwareError {
    fn from(value: blelog_format::RecordError) -> Self {
        Self::Record(value)
    }
}

impl fmt::Display for FirmwareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for FirmwareError {}

pub type Result<T> = core::result::Result<T, FirmwareError>;
