use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Default, Debug)]
pub struct CacheStats {
    read_hits: AtomicU64,
    read_misses: AtomicU64,
    write_hits: AtomicU64,
    write_misses: AtomicU64,
    evictions: AtomicU64,
    dirty_flushes: AtomicU64,
}

impl CacheStats {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_read_hit(&self) {
        self.read_hits.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_read_miss(&self) {
        self.read_misses.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_write_hit(&self) {
        self.write_hits.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_write_miss(&self) {
        self.write_misses.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_eviction(&self) {
        self.evictions.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_dirty_flush(&self) {
        self.dirty_flushes.fetch_add(1, Ordering::Relaxed);
    }

    fn hit_ratio(hits: u64, misses: u64) -> f64 {
        if hits + misses == 0 {
            return 0.0;
        }
        hits as f64 / (hits + misses) as f64 * 100.0
    }

    pub fn print(&self, capacity: usize, peak_occupancy: usize) {
        let rh = self.read_hits.load(Ordering::Relaxed);
        let rm = self.read_misses.load(Ordering::Relaxed);
        let wh = self.write_hits.load(Ordering::Relaxed);
        let wm = self.write_misses.load(Ordering::Relaxed);
        let ev = self.evictions.load(Ordering::Relaxed);
        let df = self.dirty_flushes.load(Ordering::Relaxed);

        println!("=========== Cache Statistics ===========");
        println!("  Read Hits          : {:>10}", rh);
        println!("  Read Misses        : {:>10}", rm);
        println!("  Read Hit Ratio     : {:>9.2}%", Self::hit_ratio(rh, rm));
        println!("  Write Hits         : {:>10}", wh);
        println!("  Write Misses       : {:>10}", wm);
        println!("  Write Hit Ratio    : {:>9.2}%", Self::hit_ratio(wh, wm));
        println!("  Evictions          : {:>10}", ev);
        println!("  Dirty Flushes      : {:>10}", df);
        println!("  Capacity           : {:>10}", capacity);
        println!("  Peak Occupancy     : {:>10}", peak_occupancy);
        println!("========================================");
    }
}
