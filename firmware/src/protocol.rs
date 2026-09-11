use std::ffi::c_void;

use blelog_format::{
    record::FORMAT_VERSION,
    transport::{
        decode_frame, encode_frame, encode_status, message, response_type, Frame, Status,
        MAX_DECODED_FRAME_LEN, MAX_ENCODED_FRAME_LEN,
    },
    wire::{
        decode_dump_read_request, decode_i64, decode_u32, encode_dump_begin,
        encode_dump_read_response, Info, StartCapture,
    },
};
use esp_idf_svc::sys;

use crate::{
    controller::Controller,
    error::FirmwareError,
    hardware::{EXPECTED_PARTITION_SIZE, WARN_PARTITION},
};

pub struct UsbSerialJtag;

impl UsbSerialJtag {
    pub fn install() -> Result<Self, FirmwareError> {
        let mut config = sys::usb_serial_jtag_driver_config_t {
            tx_buffer_size: 8 * 1024,
            rx_buffer_size: 1024,
        };
        let rc = unsafe { sys::usb_serial_jtag_driver_install(&mut config) };
        if rc == sys::ESP_OK {
            Ok(Self)
        } else {
            Err(FirmwareError::Usb(rc))
        }
    }

    fn read(&mut self, output: &mut [u8]) -> Result<usize, FirmwareError> {
        let count = unsafe {
            sys::usb_serial_jtag_read_bytes(
                output.as_mut_ptr().cast::<c_void>(),
                output.len() as u32,
                10,
            )
        };
        if count < 0 {
            Err(FirmwareError::Usb(count))
        } else {
            Ok(count as usize)
        }
    }

    fn write_all(&mut self, mut bytes: &[u8]) -> Result<(), FirmwareError> {
        while !bytes.is_empty() {
            let written = unsafe {
                sys::usb_serial_jtag_write_bytes(bytes.as_ptr().cast::<c_void>(), bytes.len(), 100)
            };
            if written <= 0 {
                return Err(FirmwareError::Usb(written));
            }
            bytes = &bytes[written as usize..];
        }
        Ok(())
    }
}

impl Drop for UsbSerialJtag {
    fn drop(&mut self) {
        unsafe {
            sys::usb_serial_jtag_driver_uninstall();
        }
    }
}

pub fn run(mut usb: UsbSerialJtag, controller: &mut Controller) -> ! {
    let mut encoded_input = Vec::with_capacity(MAX_ENCODED_FRAME_LEN);
    let mut read_buffer = [0u8; 128];
    let mut decoded = vec![0u8; MAX_DECODED_FRAME_LEN];
    let mut encoded_output = vec![0u8; MAX_ENCODED_FRAME_LEN];
    let mut discarding_oversize = false;

    loop {
        let Ok(count) = usb.read(&mut read_buffer) else {
            continue;
        };
        for &byte in &read_buffer[..count] {
            if byte != 0 {
                if !discarding_oversize {
                    if encoded_input.len() == MAX_ENCODED_FRAME_LEN {
                        encoded_input.clear();
                        discarding_oversize = true;
                    } else {
                        encoded_input.push(byte);
                    }
                }
                continue;
            }
            if discarding_oversize || encoded_input.is_empty() {
                encoded_input.clear();
                discarding_oversize = false;
                continue;
            }
            let frame = decode_frame(&encoded_input, &mut decoded);
            encoded_input.clear();
            let Ok(frame) = frame else {
                continue;
            };
            let (status, detail) = dispatch(controller, frame.message_type, frame.payload);
            let mut response_payload = vec![0u8; detail.len() + 2];
            let payload_len = encode_status(status, &detail, &mut response_payload).unwrap();
            let response = Frame {
                message_type: response_type(frame.message_type),
                request_id: frame.request_id,
                payload: &response_payload[..payload_len],
            };
            if let Ok(length) = encode_frame(&response, &mut encoded_output) {
                let _ = usb.write_all(&encoded_output[..length]);
            }
        }
    }
}

fn dispatch(controller: &mut Controller, command: u8, payload: &[u8]) -> (Status, Vec<u8>) {
    match dispatch_inner(controller, command, payload) {
        Ok(detail) => (Status::Ok, detail),
        Err(error) => (status_for(error), format!("{error}").into_bytes()),
    }
}

fn dispatch_inner(
    controller: &mut Controller,
    command: u8,
    payload: &[u8],
) -> Result<Vec<u8>, FirmwareError> {
    match command {
        message::PING => {
            require_empty(payload)?;
            Ok(b"BLELOG1".to_vec())
        }
        message::INFO => {
            require_empty(payload)?;
            let snapshot = controller.snapshot()?;
            let mut warnings = controller.hardware.warning_flags;
            if snapshot.partition_size != EXPECTED_PARTITION_SIZE {
                warnings |= WARN_PARTITION;
            }
            let info = Info {
                record_format: FORMAT_VERSION,
                state: snapshot.state,
                scan_mode: controller.scan_settings.scan_mode,
                extended_adv_enabled: true,
                flash_size: controller.hardware.flash_size,
                psram_size: controller.hardware.psram_size,
                partition_size: snapshot.partition_size,
                valid_bytes: snapshot.valid_bytes,
                free_bytes: snapshot.free_bytes(),
                record_count: snapshot.record_count,
                base_mac: controller.hardware.base_mac,
                warning_flags: warnings,
                scan_interval_ms: controller.scan_settings.interval_ms,
                scan_window_ms: controller.scan_settings.window_ms,
                chip_model: controller.hardware.chip_model,
                firmware_version: env!("CARGO_PKG_VERSION"),
            };
            let mut bytes = vec![0u8; 128];
            let len = info.encode(&mut bytes).map_err(|_| FirmwareError::Bounds)?;
            bytes.truncate(len);
            Ok(bytes)
        }
        message::STATS => {
            require_empty(payload)?;
            let mut bytes = vec![0u8; blelog_format::wire::STATS_LEN];
            controller
                .storage
                .stats
                .snapshot()
                .encode(&mut bytes)
                .map_err(|_| FirmwareError::Bounds)?;
            Ok(bytes)
        }
        message::START_CAPTURE => {
            let settings = StartCapture::decode(payload).map_err(|_| FirmwareError::Bounds)?;
            controller.start_capture(settings)?;
            Ok(Vec::new())
        }
        message::STOP_CAPTURE => {
            require_empty(payload)?;
            controller.stop_capture()?;
            Ok(Vec::new())
        }
        message::SET_CLOCK => {
            let unix_ms = decode_i64(payload).map_err(|_| FirmwareError::Bounds)?;
            let monotonic_us = controller.set_clock(unix_ms)?;
            Ok(monotonic_us.to_le_bytes().to_vec())
        }
        message::DUMP_BEGIN => {
            require_empty(payload)?;
            let (transfer_id, bytes, records) = controller.storage.dump_begin()?;
            let mut response = vec![0u8; 16];
            encode_dump_begin(transfer_id, bytes, records, &mut response)
                .map_err(|_| FirmwareError::Bounds)?;
            Ok(response)
        }
        message::DUMP_READ => {
            let (transfer_id, offset, length) =
                decode_dump_read_request(payload).map_err(|_| FirmwareError::Bounds)?;
            let bytes = controller.storage.dump_read(transfer_id, offset, length)?;
            let mut response = vec![0u8; bytes.len() + 10];
            let len = encode_dump_read_response(transfer_id, offset, &bytes, &mut response)
                .map_err(|_| FirmwareError::Bounds)?;
            response.truncate(len);
            Ok(response)
        }
        message::DUMP_END => {
            let transfer_id = decode_u32(payload).map_err(|_| FirmwareError::Bounds)?;
            controller.storage.dump_end(transfer_id)?;
            Ok(Vec::new())
        }
        message::ERASE_LOG => {
            require_empty(payload)?;
            controller.storage.erase()?;
            Ok(Vec::new())
        }
        _ => Err(FirmwareError::UnsupportedFormat(command)),
    }
}

fn require_empty(payload: &[u8]) -> Result<(), FirmwareError> {
    if payload.is_empty() {
        Ok(())
    } else {
        Err(FirmwareError::Bounds)
    }
}

fn status_for(error: FirmwareError) -> Status {
    match error {
        FirmwareError::Bounds => Status::InvalidPayload,
        FirmwareError::InvalidState => Status::InvalidState,
        FirmwareError::Busy => Status::Busy,
        FirmwareError::Full => Status::Full,
        FirmwareError::CorruptTail => Status::CorruptTail,
        FirmwareError::UnsupportedFormat(_) => Status::UnknownCommand,
        FirmwareError::FlashRead(_)
        | FirmwareError::FlashWrite(_)
        | FirmwareError::FlashErase(_)
        | FirmwareError::Usb(_) => Status::Io,
        FirmwareError::Record(_) => Status::UnsupportedFormat,
        _ => Status::Io,
    }
}
