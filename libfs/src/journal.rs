use std::sync::{Arc, Mutex};

use crate::{
    disk::Disk,
    layout::{JOURNAL_MAGIC, JournalHeader, MAX_JOURNAL_BLOCKS},
};

pub struct Journal {
    disk: Arc<Mutex<Disk>>,
    journal_start_block: u64,
}

impl Journal {
    pub fn new(disk: Arc<Mutex<Disk>>, journal_start_block: u64) -> Self {
        Journal {
            disk,
            journal_start_block,
        }
    }

    //write all blocks + commit marker to journal before applying real writes
    pub fn begin_transaction(&mut self, writes: &[(u64, [u8; 4096])]) {
        assert!(
            writes.len() <= MAX_JOURNAL_BLOCKS,
            "too many blocks in one transaction"
        );

        let mut block_numbers = [0u64; MAX_JOURNAL_BLOCKS];
        for (i, (block_num, _)) in writes.iter().enumerate() {
            block_numbers[i] = *block_num;
        }

        let header = JournalHeader {
            magic: JOURNAL_MAGIC,
            commited: 1,
            num_blocks: writes.len() as u8,
            _padding: [0u8; 2],
            block_numbers,
        };

        //write header to journal block 0
        let mut header_block = [0u8; 4096];

        unsafe {
            std::ptr::write(header_block.as_mut_ptr() as *mut JournalHeader, header);
        }

        self.disk
            .lock()
            .unwrap()
            .write_block(self.journal_start_block, &header_block)
            .unwrap();

        //write each block's data to journal blocks 1..N
        for (i, (_, data)) in writes.iter().enumerate() {
            let journal_block = self.journal_start_block + 1 + i as u64;
            self.disk
                .lock()
                .unwrap()
                .write_block(journal_block, data)
                .unwrap();
        }
        self.disk.lock().unwrap().flush_all();
    }

    //clear the journal after real writes
    pub fn commit_complete(&mut self) {
        let empty_header = [0u8; 4096]; //magic = 0 means "no pending transation"
        self.disk
            .lock()
            .unwrap()
            .write_block(self.journal_start_block, &empty_header)
            .unwrap();
    }

    //called at mount time - replay any pending transaction
    pub fn recover(&mut self) {
        let header_block = self
            .disk
            .lock()
            .unwrap()
            .read_block(self.journal_start_block)
            .unwrap();
        let header = unsafe { std::ptr::read(header_block.as_ptr() as *const JournalHeader) };

        if header.magic != JOURNAL_MAGIC || header.commited == 0 {
            return;
        }

        println!(
            "Pending transaction found, replaying {} blocks",
            header.num_blocks
        );

        for i in 0..header.num_blocks as usize {
            let target_block = header.block_numbers[i];
            let journal_block = self.journal_start_block + 1 + i as u64;
            let data = self.disk.lock().unwrap().read_block(journal_block).unwrap();
            self.disk
                .lock()
                .unwrap()
                .write_block(target_block, &data)
                .unwrap();
        }

        self.disk.lock().unwrap().flush_all();
        self.commit_complete();
        print!("Journal recovery complete")
    }
}
