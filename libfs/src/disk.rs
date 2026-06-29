use std::collections::{HashMap, VecDeque};
use std::fs::OpenOptions;
use std::io::{self, Read, Seek, SeekFrom, Write};

const CACHE_CAPACITY: usize = 256;

struct CacheEntry {
    data: [u8; 4096],
    dirty: bool,
}
pub struct Disk {
    file: std::fs::File,
    cache: HashMap<u64, CacheEntry>,
    lru_order: VecDeque<u64>,
}

impl Disk {
    pub fn create(path: &str, size: u64) -> io::Result<Disk> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        file.set_len(size)?;
        Ok(Disk {
            file,
            cache: HashMap::new(),
            lru_order: VecDeque::new(),
        })
    }
    //open an existing file
    pub fn open(path: &str) -> io::Result<Disk> {
        let file1 = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?;
        Ok(Disk {
            file: file1,
            cache: HashMap::new(),
            lru_order: VecDeque::new(),
        })
    }

    fn touch_lru(&mut self, block_num: u64) {
        self.lru_order.retain(|&b| b != block_num);
        self.lru_order.push_back(block_num);
    }

    //read one block from disk
    pub fn read_block(&mut self, block_num: u64) -> io::Result<[u8; 4096]> {
        if let Some(entry) = self.cache.get(&block_num) {
            let data = entry.data;
            self.touch_lru(block_num);
            return Ok(data);
        }

        let offset = block_num * 4096;
        self.file.seek(SeekFrom::Start(offset))?;
        let mut buf = [0u8; 4096];
        self.file.read_exact(&mut buf)?;

        self.insert_into_cache(block_num, buf, false);
        Ok(buf)
    }

    //write one block to disk
    pub fn write_block(&mut self, block_num: u64, data: &[u8; 4096]) -> io::Result<()> {
        self.insert_into_cache(block_num, *data, true);
        Ok(())
    }

    fn insert_into_cache(&mut self, block_num: u64, data: [u8; 4096], dirty: bool) {
        if let Some(entry) = self.cache.get_mut(&block_num) {
            entry.data = data;
            entry.dirty = entry.dirty || dirty;
        } else {
            if self.cache.len() >= CACHE_CAPACITY {
                self.evict_one();
            }
            self.cache.insert(block_num, CacheEntry { data, dirty });
        }
        self.touch_lru(block_num);
    }

    fn evict_one(&mut self) {
        if let Some(victim) = self.lru_order.pop_front() {
            self.flush_block(victim);
            self.cache.remove(&victim);
        }
    }

    fn flush_block(&mut self, block_num: u64) {
        let needs_flush = self.cache.get(&block_num).map_or(false, |e| e.dirty);
        if needs_flush {
            let data = self.cache.get(&block_num).unwrap().data;
            let offset = block_num * 4096;
            self.file.seek(SeekFrom::Start(offset)).unwrap();
            self.file.write_all(&data).unwrap();
            if let Some(entry) = self.cache.get_mut(&block_num) {
                entry.dirty = false;
            }
        }
    }

    pub fn flush_all(&mut self) {
        let dirty_blocks: Vec<u64> = self
            .cache
            .iter()
            .filter(|(_, e)| e.dirty)
            .map(|(&b, _)| b)
            .collect();
        for block_num in dirty_blocks {
            self.flush_block(block_num);
        }
    }
}
