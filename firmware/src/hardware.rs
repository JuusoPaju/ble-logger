use esp_idf_svc::sys;

pub const EXPECTED_FLASH_SIZE: u32 = 16 * 1024 * 1024;
pub const EXPECTED_PSRAM_SIZE: u32 = 8 * 1024 * 1024;
pub const EXPECTED_PARTITION_SIZE: u32 = 0x00CF_0000;

pub const WARN_CHIP: u16 = 1 << 0;
pub const WARN_FLASH: u16 = 1 << 1;
pub const WARN_PSRAM: u16 = 1 << 2;
pub const WARN_PARTITION: u16 = 1 << 3;

#[derive(Clone, Copy, Debug)]
pub struct HardwareInfo {
    pub chip_model: &'static str,
    pub base_mac: [u8; 6],
    pub flash_size: u32,
    pub psram_size: u32,
    pub warning_flags: u16,
}

impl HardwareInfo {
    pub fn detect() -> Self {
        let mut chip = EspChipInfo::default();
        unsafe {
            esp_chip_info(&mut chip);
        }
        let chip_ok = chip.model == 9;

        let mut flash_size = 0u32;
        let flash_rc =
            unsafe { sys::esp_flash_get_size(sys::esp_flash_default_chip, &mut flash_size) };
        if flash_rc != sys::ESP_OK {
            flash_size = 0;
        }

        let psram_size = if unsafe { sys::esp_psram_is_initialized() } {
            unsafe { sys::esp_psram_get_size() as u32 }
        } else {
            0
        };

        let mut base_mac = [0u8; 6];
        let mac_rc =
            unsafe { sys::esp_read_mac(base_mac.as_mut_ptr(), sys::esp_mac_type_t_ESP_MAC_BASE) };
        if mac_rc != sys::ESP_OK {
            base_mac = [0; 6];
        }

        let mut warning_flags = 0;
        if !chip_ok {
            warning_flags |= WARN_CHIP;
        }
        if flash_size != EXPECTED_FLASH_SIZE {
            warning_flags |= WARN_FLASH;
        }
        if psram_size != EXPECTED_PSRAM_SIZE {
            warning_flags |= WARN_PSRAM;
        }

        Self {
            chip_model: if chip_ok { "ESP32-S3" } else { "unexpected" },
            base_mac,
            flash_size,
            psram_size,
            warning_flags,
        }
    }
}

// esp_chip_info.h is not part of esp-idf-sys' default bindgen header set in
// 0.37.2, so declare this stable ESP-IDF C ABI locally.
#[repr(C)]
#[derive(Default)]
struct EspChipInfo {
    model: i32,
    features: u32,
    revision: u16,
    cores: u8,
}

extern "C" {
    fn esp_chip_info(out_info: *mut EspChipInfo);
}
