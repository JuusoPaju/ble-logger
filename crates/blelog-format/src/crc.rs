//! CRC-32/ISO-HDLC (`CRC-32`, Ethernet) without lookup tables or allocation.

pub const CHECK_123456789: u32 = 0xCBF4_3926;

#[derive(Clone, Copy, Debug)]
pub struct Crc32(u32);

impl Crc32 {
    pub const fn new() -> Self {
        Self(0xFFFF_FFFF)
    }

    pub fn update(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 ^= u32::from(byte);
            for _ in 0..8 {
                let mask = 0u32.wrapping_sub(self.0 & 1);
                self.0 = (self.0 >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
    }

    pub const fn finalize(self) -> u32 {
        !self.0
    }
}

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = Crc32::new();
    crc.update(bytes);
    crc.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_check_value() {
        assert_eq!(crc32(b"123456789"), CHECK_123456789);
    }
}
