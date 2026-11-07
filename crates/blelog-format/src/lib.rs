#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]

//! Stable, allocation-free codecs shared by the ESP32-S3 firmware and host CLI.

pub mod capture;
pub mod crc;
pub mod record;
pub mod transport;
pub mod wire;

pub use record::{
    decode_record, encode_record, scan_stream, BleObservation, ClockSync, DecodedRecord, Record,
    RecordError, ScanMode, SessionEnd, SessionStart, StreamScan,
};
