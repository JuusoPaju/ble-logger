use core::fmt;

use crate::crc::crc32;

pub const PROTOCOL_VERSION: u8 = 1;
pub const FRAME_HEADER_LEN: usize = 8;
pub const FRAME_CRC_LEN: usize = 4;
pub const MAX_COMMAND_PAYLOAD: usize = 4096 + 16;
pub const MAX_DECODED_FRAME_LEN: usize = FRAME_HEADER_LEN + MAX_COMMAND_PAYLOAD + FRAME_CRC_LEN;
pub const MAX_ENCODED_FRAME_LEN: usize = MAX_DECODED_FRAME_LEN + (MAX_DECODED_FRAME_LEN / 254) + 2;

pub mod message {
    pub const PING: u8 = 0x01;
    pub const INFO: u8 = 0x02;
    pub const STATS: u8 = 0x03;
    pub const START_CAPTURE: u8 = 0x04;
    pub const STOP_CAPTURE: u8 = 0x05;
    pub const SET_CLOCK: u8 = 0x06;
    pub const DUMP_BEGIN: u8 = 0x07;
    pub const DUMP_READ: u8 = 0x08;
    pub const DUMP_END: u8 = 0x09;
    pub const ERASE_LOG: u8 = 0x0A;
    pub const RESPONSE_BIT: u8 = 0x80;
}

#[repr(u16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ok = 0,
    InvalidFrame = 1,
    UnsupportedVersion = 2,
    UnknownCommand = 3,
    InvalidPayload = 4,
    InvalidState = 5,
    Busy = 6,
    Bounds = 7,
    Io = 8,
    Full = 9,
    CorruptTail = 10,
    UnsupportedFormat = 11,
}

impl TryFrom<u16> for Status {
    type Error = FrameError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Ok(match value {
            0 => Self::Ok,
            1 => Self::InvalidFrame,
            2 => Self::UnsupportedVersion,
            3 => Self::UnknownCommand,
            4 => Self::InvalidPayload,
            5 => Self::InvalidState,
            6 => Self::Busy,
            7 => Self::Bounds,
            8 => Self::Io,
            9 => Self::Full,
            10 => Self::CorruptTail,
            11 => Self::UnsupportedFormat,
            _ => return Err(FrameError::InvalidStatus(value)),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame<'a> {
    pub message_type: u8,
    pub request_id: u16,
    pub payload: &'a [u8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    BufferTooSmall,
    PayloadTooLong,
    EncodedFrameTooLong,
    MalformedCobs,
    Truncated,
    UnsupportedVersion(u8),
    LengthMismatch,
    CrcMismatch { expected: u32, actual: u32 },
    InvalidStatus(u16),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

#[cfg(feature = "std")]
impl std::error::Error for FrameError {}

pub fn response_type(request_type: u8) -> u8 {
    request_type | message::RESPONSE_BIT
}

pub fn encode_frame(frame: &Frame<'_>, output: &mut [u8]) -> Result<usize, FrameError> {
    if frame.payload.len() > MAX_COMMAND_PAYLOAD {
        return Err(FrameError::PayloadTooLong);
    }
    let raw_len = FRAME_HEADER_LEN + frame.payload.len() + FRAME_CRC_LEN;
    let needed = raw_len + raw_len / 254 + 2;
    if output.len() < needed {
        return Err(FrameError::BufferTooSmall);
    }

    // Constructing into the end of the caller's buffer keeps this allocation-free
    // and allows the COBS result to use the beginning of that same buffer.
    let raw_start = output.len() - raw_len;
    let raw = &mut output[raw_start..];
    raw[0] = PROTOCOL_VERSION;
    raw[1] = frame.message_type;
    raw[2..4].copy_from_slice(&frame.request_id.to_le_bytes());
    raw[4..8].copy_from_slice(&(frame.payload.len() as u32).to_le_bytes());
    raw[8..8 + frame.payload.len()].copy_from_slice(frame.payload);
    let crc_offset = raw_len - FRAME_CRC_LEN;
    let crc = crc32(&raw[..crc_offset]);
    raw[crc_offset..].copy_from_slice(&crc.to_le_bytes());

    // The ranges overlap only in the caller's slice. COBS reads ahead of its
    // write position because raw_start is greater than the maximum encoded size.
    // Use a small fixed scratch because the maximum frame size is bounded.
    let mut scratch = [0u8; MAX_DECODED_FRAME_LEN];
    scratch[..raw_len].copy_from_slice(raw);
    let encoded_len = cobs_encode(&scratch[..raw_len], output)?;
    output[encoded_len] = 0;
    Ok(encoded_len + 1)
}

/// Decode a single COBS frame. `encoded` must not include the `0x00` delimiter.
/// The returned payload borrows from `scratch`.
pub fn decode_frame<'a>(encoded: &[u8], scratch: &'a mut [u8]) -> Result<Frame<'a>, FrameError> {
    if encoded.len() > MAX_ENCODED_FRAME_LEN {
        return Err(FrameError::EncodedFrameTooLong);
    }
    let len = cobs_decode(encoded, scratch)?;
    if len < FRAME_HEADER_LEN + FRAME_CRC_LEN {
        return Err(FrameError::Truncated);
    }
    if scratch[0] != PROTOCOL_VERSION {
        return Err(FrameError::UnsupportedVersion(scratch[0]));
    }
    let message_type = scratch[1];
    let request_id = u16::from_le_bytes([scratch[2], scratch[3]]);
    let payload_len = u32::from_le_bytes(scratch[4..8].try_into().unwrap()) as usize;
    if payload_len > MAX_COMMAND_PAYLOAD || len != FRAME_HEADER_LEN + payload_len + FRAME_CRC_LEN {
        return Err(FrameError::LengthMismatch);
    }
    let crc_offset = len - FRAME_CRC_LEN;
    let expected = u32::from_le_bytes(scratch[crc_offset..len].try_into().unwrap());
    let actual = crc32(&scratch[..crc_offset]);
    if expected != actual {
        return Err(FrameError::CrcMismatch { expected, actual });
    }
    Ok(Frame {
        message_type,
        request_id,
        payload: &scratch[FRAME_HEADER_LEN..crc_offset],
    })
}

pub fn encode_status(
    status: Status,
    detail: &[u8],
    output: &mut [u8],
) -> Result<usize, FrameError> {
    if output.len() < detail.len() + 2 {
        return Err(FrameError::BufferTooSmall);
    }
    output[..2].copy_from_slice(&(status as u16).to_le_bytes());
    output[2..2 + detail.len()].copy_from_slice(detail);
    Ok(detail.len() + 2)
}

pub fn decode_status(payload: &[u8]) -> Result<(Status, &[u8]), FrameError> {
    if payload.len() < 2 {
        return Err(FrameError::Truncated);
    }
    let status = Status::try_from(u16::from_le_bytes([payload[0], payload[1]]))?;
    Ok((status, &payload[2..]))
}

fn cobs_encode(input: &[u8], output: &mut [u8]) -> Result<usize, FrameError> {
    if output.is_empty() {
        return Err(FrameError::BufferTooSmall);
    }
    let mut read = 0;
    let mut write = 1;
    let mut code_index = 0;
    let mut code = 1u8;
    while read < input.len() {
        if input[read] == 0 {
            if code_index >= output.len() {
                return Err(FrameError::BufferTooSmall);
            }
            output[code_index] = code;
            code_index = write;
            write += 1;
            code = 1;
        } else {
            if write >= output.len() {
                return Err(FrameError::BufferTooSmall);
            }
            output[write] = input[read];
            write += 1;
            code += 1;
            if code == 0xFF {
                output[code_index] = code;
                code_index = write;
                write += 1;
                code = 1;
            }
        }
        read += 1;
    }
    if code_index >= output.len() {
        return Err(FrameError::BufferTooSmall);
    }
    output[code_index] = code;
    Ok(write)
}

fn cobs_decode(input: &[u8], output: &mut [u8]) -> Result<usize, FrameError> {
    let mut read = 0;
    let mut write = 0;
    while read < input.len() {
        let code = input[read];
        if code == 0 {
            return Err(FrameError::MalformedCobs);
        }
        read += 1;
        let run = usize::from(code - 1);
        if read + run > input.len() || write + run > output.len() {
            return Err(if write + run > output.len() {
                FrameError::BufferTooSmall
            } else {
                FrameError::MalformedCobs
            });
        }
        output[write..write + run].copy_from_slice(&input[read..read + run]);
        read += run;
        write += run;
        if code != 0xFF && read < input.len() {
            if write >= output.len() {
                return Err(FrameError::BufferTooSmall);
            }
            output[write] = 0;
            write += 1;
        }
    }
    Ok(write)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trip_with_zeroes_and_long_run() {
        let mut payload = [1u8; 300];
        payload[0] = 0;
        payload[254] = 0;
        let frame = Frame {
            message_type: message::DUMP_READ,
            request_id: 42,
            payload: &payload,
        };
        let mut encoded = [0u8; MAX_ENCODED_FRAME_LEN];
        let encoded_len = encode_frame(&frame, &mut encoded).unwrap();
        assert_eq!(encoded[encoded_len - 1], 0);
        let mut decoded = [0u8; MAX_DECODED_FRAME_LEN];
        let got = decode_frame(&encoded[..encoded_len - 1], &mut decoded).unwrap();
        assert_eq!(got, frame);
    }

    #[test]
    fn rejects_crc_and_length_damage() {
        let frame = Frame {
            message_type: message::PING,
            request_id: 7,
            payload: b"ping",
        };
        let mut encoded = [0u8; 64];
        let len = encode_frame(&frame, &mut encoded).unwrap();
        encoded[2] ^= 1;
        let mut decoded = [0u8; 64];
        assert!(decode_frame(&encoded[..len - 1], &mut decoded).is_err());
    }

    #[test]
    fn fragmented_input_reassembles_at_delimiter() {
        let frame = Frame {
            message_type: message::INFO,
            request_id: 99,
            payload: b"abc\0def",
        };
        let mut encoded = [0u8; 128];
        let len = encode_frame(&frame, &mut encoded).unwrap();
        let mut accumulated = std::vec::Vec::new();
        for fragment in encoded[..len].chunks(2) {
            for &byte in fragment {
                if byte == 0 {
                    let mut scratch = [0u8; 128];
                    assert_eq!(decode_frame(&accumulated, &mut scratch).unwrap(), frame);
                    accumulated.clear();
                } else {
                    accumulated.push(byte);
                }
            }
        }
        assert!(accumulated.is_empty());
    }

    #[test]
    fn rejects_garbage_and_oversized_frames() {
        let mut scratch = [0u8; MAX_DECODED_FRAME_LEN];
        assert!(decode_frame(b"ESP-ROM:boot text", &mut scratch).is_err());
        let oversized = std::vec![1u8; MAX_ENCODED_FRAME_LEN + 1];
        assert_eq!(
            decode_frame(&oversized, &mut scratch),
            Err(FrameError::EncodedFrameTooLong)
        );
    }
}
