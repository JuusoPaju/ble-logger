use core::sync::atomic::{AtomicU32, Ordering};

use blelog_format::capture::{RadioStats, RADIO_STATS_FIELDS};
use blelog_format::wire::Stats;

#[derive(Default)]
pub struct AtomicStats {
    pub received: AtomicU32,
    pub stored: AtomicU32,
    pub queue_drops: AtomicU32,
    pub full_drops: AtomicU32,
    pub flash_errors: AtomicU32,
    pub bytes_written: AtomicU32,
    pub queue_high_water: AtomicU32,
    pub radio: [AtomicU32; RADIO_STATS_FIELDS],
    pub active_radio: AtomicU32,
    pub active_channel: AtomicU32,
}

impl AtomicStats {
    pub fn radio_snapshot(&self) -> RadioStats {
        RadioStats(core::array::from_fn(|i| {
            u64::from(self.radio[i].load(Ordering::Relaxed))
                * if (7..=9).contains(&i) { 1000 } else { 1 }
        }))
    }
    pub fn reset_radio(&self) {
        for value in &self.radio {
            value.store(0, Ordering::Relaxed);
        }
    }
    pub fn snapshot(&self) -> Stats {
        Stats {
            received_observations: u64::from(self.received.load(Ordering::Relaxed)),
            stored_observations: u64::from(self.stored.load(Ordering::Relaxed)),
            dropped_queue_full: u64::from(self.queue_drops.load(Ordering::Relaxed)),
            dropped_storage_full: u64::from(self.full_drops.load(Ordering::Relaxed)),
            flash_write_errors: u64::from(self.flash_errors.load(Ordering::Relaxed)),
            bytes_written: u64::from(self.bytes_written.load(Ordering::Relaxed)),
            queue_high_water: self.queue_high_water.load(Ordering::Relaxed),
        }
    }

    pub fn observe_queue_depth(&self, depth: usize) {
        let depth = depth.min(u32::MAX as usize) as u32;
        self.queue_high_water.fetch_max(depth, Ordering::Relaxed);
    }
}
