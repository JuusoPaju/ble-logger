use std::{fs, path::Path};

use anyhow::{bail, Context, Result};
use blelog_format::record::{scan_stream, StreamScan, StreamTail};

pub fn verify_bytes(bytes: &[u8], allow_erased_tail: bool) -> Result<StreamScan> {
    let scan = scan_stream(bytes);
    match scan.tail {
        StreamTail::ExactEnd => {
            analyze_wifi(&bytes[..scan.valid_bytes])?;
            Ok(scan)
        }
        StreamTail::Erased if allow_erased_tail => {
            analyze_wifi(&bytes[..scan.valid_bytes])?;
            Ok(scan)
        }
        StreamTail::Erased => bail!(
            "file contains an erased tail after byte {}",
            scan.valid_bytes
        ),
        StreamTail::Corrupt(error) => bail!("invalid record at byte {}: {error}", scan.valid_bytes),
        StreamTail::UnsupportedVersion(version) => {
            bail!(
                "unsupported record version {version} at byte {}",
                scan.valid_bytes
            )
        }
    }
}

#[derive(Default, Debug)]
pub struct WifiSummary {
    pub frames: u64,
    pub incomplete: u64,
    pub metadata_only: u64,
    pub unknown_records: u64,
}
pub fn analyze_wifi(mut bytes: &[u8]) -> Result<WifiSummary> {
    use blelog_format::{
        capture::{CaptureRecord, WIFI_METADATA_ONLY},
        DecodedRecord,
    };
    let mut assembler = crate::wifi::Assembler::default();
    let mut summary = WifiSummary::default();
    while !bytes.is_empty() {
        let decoded = blelog_format::decode_record(bytes)?;
        match decoded.record {
            DecodedRecord::Capture(CaptureRecord::Wifi(fragment)) => {
                if let Some(frame) = assembler.push(fragment)? {
                    summary.frames += 1;
                    if frame.meta.flags & WIFI_METADATA_ONLY != 0 {
                        summary.metadata_only += 1;
                    }
                }
            }
            DecodedRecord::SessionStart(_) | DecodedRecord::SessionEnd(_) => assembler.boundary(),
            DecodedRecord::Unknown { .. } => summary.unknown_records += 1,
            _ => {}
        }
        bytes = &bytes[decoded.total_len..];
    }
    assembler.boundary();
    summary.incomplete = assembler.incomplete;
    Ok(summary)
}

pub fn verify_file(path: &Path) -> Result<StreamScan> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    verify_bytes(&bytes, false)
}
