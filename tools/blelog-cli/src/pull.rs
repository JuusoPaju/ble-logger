use std::{
    fs::{self, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use blelog_format::{
    record::{scan_stream, RecordError, StreamTail, FORMAT_VERSION},
    transport::message,
    wire::{
        decode_dump_begin, decode_dump_read_response, encode_dump_read_request, encode_u32, Info,
        MAX_DUMP_CHUNK,
    },
};

use crate::{verify::verify_file, RequestClient};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PullSummary {
    pub bytes: u64,
    pub records: u64,
    pub observations: u64,
}

pub fn partial_path(output: &Path) -> PathBuf {
    let mut name = output.as_os_str().to_os_string();
    name.push(".part");
    PathBuf::from(name)
}

pub fn pull<C: RequestClient>(
    client: &mut C,
    output: &Path,
    resume: bool,
    chunk_size: u16,
) -> Result<PullSummary> {
    if chunk_size == 0 || chunk_size > MAX_DUMP_CHUNK {
        bail!("chunk size must be in 1..={MAX_DUMP_CHUNK}");
    }
    let info_bytes = client.request(message::INFO, &[]).context("INFO failed")?;
    let info = Info::decode(&info_bytes)?;
    if info.record_format != FORMAT_VERSION {
        bail!("device record format {} is unsupported", info.record_format);
    }
    // STOP is intentionally separate from DUMP_BEGIN: firmware rejects dump while active.
    client
        .request(message::STOP_CAPTURE, &[])
        .context("failed to stop capture")?;
    let begin = client
        .request(message::DUMP_BEGIN, &[])
        .context("DUMP_BEGIN failed")?;
    let (transfer_id, valid_bytes, expected_records) = decode_dump_begin(&begin)?;
    let part = partial_path(output);

    let result = (|| -> Result<PullSummary> {
        let mut start_offset = 0u64;
        if resume && part.exists() {
            let bytes = fs::read(&part).context("failed to read partial download")?;
            let scan = scan_stream(&bytes);
            match scan.tail {
                StreamTail::ExactEnd => start_offset = scan.valid_bytes as u64,
                StreamTail::Corrupt(RecordError::Truncated) => {
                    start_offset = scan.valid_bytes as u64;
                }
                // A download can end in an uncommitted marker only if the transfer/file
                // itself is damaged; unlike device recovery, this is not safe to hide.
                tail => bail!(
                    "cannot resume {} at byte {}: {tail:?}",
                    part.display(),
                    scan.valid_bytes
                ),
            }
            if start_offset > u64::from(valid_bytes) {
                bail!("partial file is longer than the device dump");
            }
        } else if part.exists() {
            bail!(
                "{} already exists; pass --resume or remove it",
                part.display()
            );
        }

        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(!resume)
            .open(&part)
            .with_context(|| format!("failed to open {}", part.display()))?;
        file.set_len(start_offset)?;
        file.seek(SeekFrom::Start(start_offset))?;

        let mut offset = start_offset as u32;
        while offset < valid_bytes {
            let requested = u32::from(chunk_size).min(valid_bytes - offset) as u16;
            let mut request = [0u8; 10];
            encode_dump_read_request(transfer_id, offset, requested, &mut request)?;
            let mut last_error = None;
            let mut received = None;
            for _ in 0..3 {
                match client.request(message::DUMP_READ, &request) {
                    Ok(response) => match decode_dump_read_response(&response) {
                        Ok((got_transfer, got_offset, bytes))
                            if got_transfer == transfer_id
                                && got_offset == offset
                                && !bytes.is_empty()
                                && bytes.len() <= usize::from(requested) =>
                        {
                            received = Some(bytes.to_vec());
                            break;
                        }
                        Ok(_) => last_error = Some(anyhow::anyhow!("mismatched dump chunk")),
                        Err(error) => last_error = Some(error.into()),
                    },
                    Err(error) => last_error = Some(error),
                }
            }
            let bytes = received.ok_or_else(|| {
                last_error.unwrap_or_else(|| anyhow::anyhow!("empty dump response"))
            })?;
            file.write_all(&bytes)?;
            offset += bytes.len() as u32;
        }
        file.sync_all()?;
        drop(file);

        let scan = verify_file(&part).context("downloaded record stream failed verification")?;
        if scan.valid_bytes != valid_bytes as usize || scan.record_count != expected_records {
            bail!(
                "dump changed or was incomplete: device reported {valid_bytes} bytes/{expected_records} records, got {}/{}",
                scan.valid_bytes,
                scan.record_count
            );
        }
        if output.exists() {
            bail!("refusing to replace existing {}", output.display());
        }
        fs::rename(&part, output).context("failed to atomically finalize download")?;
        Ok(PullSummary {
            bytes: valid_bytes as u64,
            records: scan.record_count,
            observations: scan.observation_count,
        })
    })();

    let mut end = [0u8; 4];
    encode_u32(transfer_id, &mut end)?;
    let end_result = client.request(message::DUMP_END, &end);
    match (result, end_result) {
        (Ok(summary), Ok(_)) => Ok(summary),
        (Ok(_), Err(error)) => Err(error).context("download succeeded but DUMP_END failed"),
        (Err(error), _) => Err(error),
    }
}
