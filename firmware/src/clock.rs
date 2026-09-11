use std::sync::Mutex;

use blelog_format::record::UNIX_MS_UNKNOWN;

#[derive(Default)]
pub struct ClockState(Mutex<Option<(i64, u64)>>);

impl ClockState {
    pub fn set(&self, unix_ms: i64, monotonic_us: u64) {
        *self.0.lock().unwrap() = Some((unix_ms, monotonic_us));
    }

    pub fn mapping(&self) -> (i64, u64) {
        self.0.lock().unwrap().unwrap_or((UNIX_MS_UNKNOWN, 0))
    }
}

pub fn monotonic_us() -> u64 {
    unsafe { esp_idf_svc::sys::esp_timer_get_time().max(0) as u64 }
}
