use std::{
    io::{Read, Write},
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Context, Result};
use blelog_format::transport::{
    decode_frame, decode_status, encode_frame, response_type, Frame, Status, MAX_DECODED_FRAME_LEN,
    MAX_ENCODED_FRAME_LEN,
};

pub trait RequestClient {
    /// Return the successful response detail (the status prefix is removed).
    fn request(&mut self, message_type: u8, payload: &[u8]) -> Result<Vec<u8>>;
}

#[derive(Debug)]
pub struct DeviceStatus(pub Status, pub String);
impl core::fmt::Display for DeviceStatus {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "device returned {:?}: {}", self.0, self.1)
    }
}
impl std::error::Error for DeviceStatus {}

pub fn supports_dual(client: &mut impl RequestClient) -> Result<bool> {
    match client.request(blelog_format::transport::message::CAPABILITIES, &[]) {
        Ok(bytes) if bytes == b"DUAL1" => Ok(true),
        Ok(_) => bail!("unsupported capability response"),
        Err(error)
            if error.downcast_ref::<DeviceStatus>().is_some_and(|e| {
                matches!(e.0, Status::UnknownCommand | Status::UnsupportedVersion)
            }) =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

pub struct SerialClient {
    port: Box<dyn serialport::SerialPort>,
    next_request_id: u16,
    response_timeout: Duration,
}

impl SerialClient {
    pub fn open(path: &str) -> Result<Self> {
        let port = serialport::new(path, 115_200)
            .timeout(Duration::from_millis(100))
            .open()
            .with_context(|| format!("failed to open {path}"))?;
        let mut client = Self {
            port,
            next_request_id: 1,
            response_timeout: Duration::from_secs(3),
        };
        client
            .wait_until_ready()
            .with_context(|| format!("BLE Logger did not become ready on {path}"))?;
        Ok(client)
    }

    #[cfg(test)]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.response_timeout = timeout;
        self
    }

    fn read_response(&mut self, request_type: u8, request_id: u16) -> Result<Vec<u8>> {
        let deadline = Instant::now() + self.response_timeout;
        let mut encoded = Vec::with_capacity(512);
        let mut byte = [0u8; 1];
        let mut scratch = [0u8; MAX_DECODED_FRAME_LEN];
        let mut discarding_oversize = false;

        while Instant::now() < deadline {
            match self.port.read(&mut byte) {
                Ok(0) => continue,
                Ok(_) if byte[0] == 0 => {
                    if discarding_oversize || encoded.is_empty() {
                        encoded.clear();
                        discarding_oversize = false;
                        continue;
                    }
                    let decoded = decode_frame(&encoded, &mut scratch);
                    encoded.clear();
                    let Ok(frame) = decoded else {
                        // Boot ROM text or a damaged frame is not a protocol response.
                        continue;
                    };
                    if frame.request_id != request_id
                        || frame.message_type != response_type(request_type)
                    {
                        continue;
                    }
                    let (status, detail) = decode_status(frame.payload)?;
                    if status != Status::Ok {
                        let explanation = String::from_utf8_lossy(detail);
                        return Err(DeviceStatus(status, explanation.into_owned()).into());
                    }
                    return Ok(detail.to_vec());
                }
                Ok(_) => {
                    if !discarding_oversize {
                        if encoded.len() == MAX_ENCODED_FRAME_LEN {
                            encoded.clear();
                            discarding_oversize = true;
                        } else {
                            encoded.push(byte[0]);
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::TimedOut => continue,
                Err(error) => return Err(error).context("serial read failed"),
            }
        }
        Err(anyhow!(
            "timed out waiting for response to command 0x{request_type:02x}"
        ))
    }

    fn wait_until_ready(&mut self) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let error = match self.request_once(blelog_format::transport::message::PING, &[]) {
                Ok(response) if response == b"BLELOG1" => return Ok(()),
                Ok(response) => anyhow!("unexpected PING response: {response:02x?}"),
                Err(error) => error,
            };
            if Instant::now() >= deadline {
                return Err(error);
            }
        }
    }

    fn request_once(&mut self, message_type: u8, payload: &[u8]) -> Result<Vec<u8>> {
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
        let frame = Frame {
            message_type,
            request_id,
            payload,
        };
        let mut encoded = [0u8; MAX_ENCODED_FRAME_LEN];
        let len = encode_frame(&frame, &mut encoded)?;
        self.port
            .write_all(&encoded[..len])
            .context("serial write failed")?;
        self.port.flush().context("serial flush failed")?;
        self.read_response(message_type, request_id)
    }
}

impl RequestClient for SerialClient {
    fn request(&mut self, message_type: u8, payload: &[u8]) -> Result<Vec<u8>> {
        let normal_timeout = self.response_timeout;
        self.response_timeout = match message_type {
            blelog_format::transport::message::STOP_CAPTURE => Duration::from_secs(30),
            blelog_format::transport::message::ERASE_LOG => Duration::from_secs(180),
            _ => normal_timeout,
        };
        let result = self.request_once(message_type, payload);
        self.response_timeout = normal_timeout;
        result
    }
}
