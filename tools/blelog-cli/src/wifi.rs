use anyhow::{bail, Result};
use blelog_format::capture::{WifiFragment, WifiMeta};

#[derive(Debug)]
pub struct WifiFrame {
    pub meta: WifiMeta,
    pub bytes: Vec<u8>,
}
#[derive(Default)]
pub struct Assembler {
    pending: Option<WifiFrame>,
    pub incomplete: u64,
}
impl Assembler {
    pub fn boundary(&mut self) {
        if self.pending.take().is_some() {
            self.incomplete += 1;
        }
    }
    pub fn push(&mut self, fragment: WifiFragment<'_>) -> Result<Option<WifiFrame>> {
        fragment.validate()?;
        if fragment.offset == 0 {
            self.boundary();
            self.pending = Some(WifiFrame {
                meta: fragment.meta,
                bytes: Vec::with_capacity(fragment.meta.captured_len as usize),
            });
        }
        let Some(frame) = self.pending.as_mut() else {
            bail!("Wi-Fi continuation without initial fragment");
        };
        if frame.meta != fragment.meta || frame.bytes.len() != fragment.offset as usize {
            bail!("inconsistent Wi-Fi fragment sequence, metadata or offset");
        }
        frame.bytes.extend_from_slice(fragment.bytes);
        if frame.bytes.len() == frame.meta.captured_len as usize {
            Ok(self.pending.take())
        } else {
            Ok(None)
        }
    }
}
