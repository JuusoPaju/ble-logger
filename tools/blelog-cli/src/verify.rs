use std::{fs, path::Path};

use anyhow::{bail, Context, Result};
use blelog_format::record::{scan_stream, StreamScan, StreamTail};

pub fn verify_bytes(bytes: &[u8], allow_erased_tail: bool) -> Result<StreamScan> {
    let scan = scan_stream(bytes);
    match scan.tail {
        StreamTail::ExactEnd => Ok(scan),
        StreamTail::Erased if allow_erased_tail => Ok(scan),
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

pub fn verify_file(path: &Path) -> Result<StreamScan> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    verify_bytes(&bytes, false)
}
