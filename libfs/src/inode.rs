use crate::disk::Disk;
use crate::layout::BLOCK_SIZE;
use crate::layout::INODE_SIZE;
use crate::layout::Inode;
use std::sync::Arc;
use std::sync::Mutex;

pub struct InodeStore {
    disk: Arc<Mutex<Disk>>,
    inode_table_block: u64,
}

impl InodeStore {
    pub fn new(disk: Arc<Mutex<Disk>>, inode_table_block: u64) -> Self {
        InodeStore {
            disk,
            inode_table_block,
        }
    }

    pub fn read_inode(&mut self, inode_num: u64) -> Inode {
        //size of each inode block is 256
        //each block size is 4096
        //total inodes in one block is 4096/256 = 16

        let inodes_per_block = BLOCK_SIZE / INODE_SIZE; //basically 16 but 
        //constans are used so if the values are changed later it needs to be updated at a single place

        let block_index = inode_num / inodes_per_block;
        let offset = ((inode_num % inodes_per_block) * 256) as usize;
        let actual_block = self.inode_table_block + block_index;

        //unwrap in Rust menas it expects the value to be definitely valid. If it is not, crash the program
        //it mainly uses Option<T> (Some(),None)
        //it also uses Result<T,E> (Ok(),Err())
        //let block = self.disk.lock().unwrap().read_block(actual_block).unwrap();
        // let mut disk = self.disk.lock().unwrap();
        // let block = disk.read_block(actual_block).unwrap();
        let block = self.disk.lock().unwrap().read_block(actual_block).unwrap();

        unsafe { std::ptr::read(block[offset..].as_ptr() as *const Inode) }
    }

    pub fn write_inode(&mut self, inode_num: u64, inode: &Inode) {
        let inodes_per_block = BLOCK_SIZE / INODE_SIZE;

        let block_index = inode_num / inodes_per_block;
        let offset = ((inode_num % inodes_per_block) * 256) as usize;
        let actual_block = self.inode_table_block + block_index;

        let mut disk = self.disk.lock().unwrap();
        let mut block = disk.read_block(actual_block).unwrap();
        //let mut block = self.disk.lock().unwrap().read_block(actual_block).unwrap();

        unsafe { std::ptr::write(block[offset..].as_mut_ptr() as *mut Inode, *inode) }

        disk.write_block(actual_block, &block).unwrap();
        //self.disk.lock().unwrap().write_block(actual_block, &block);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inode(mode: u32, uid: u32) -> Inode {
        Inode {
            mode,
            uid,
            gid: 1000,
            size: 0,
            ctime: 0,
            atime: 0,
            mtime: 0,
            hard_links: 1,
            direct: [0; 12],
            indirect: 0,
            double_indirect: 0,
            _padding: [0u8; 88],
        }
    }

    //a tiny image whose block 0 is a 16-slot inode table
    fn store(name: &str) -> (std::path::PathBuf, InodeStore) {
        let path = std::env::temp_dir().join(format!(
            "rustfs-inodetest-{}-{}.img",
            name,
            std::process::id()
        ));
        let disk = Arc::new(Mutex::new(
            Disk::create(path.to_str().unwrap(), 2 * BLOCK_SIZE).unwrap(),
        ));
        (path, InodeStore::new(disk, 0))
    }

    #[test]
    fn inode_is_exactly_one_slot() {
        assert_eq!(std::mem::size_of::<Inode>(), INODE_SIZE as usize);
    }

    //The real-world failure: inode 4 is a live directory, then a new file
    //is created in the reused inode 3 just before it. With a 264-byte Inode
    //the new inode's zero padding wiped inode 4's mode and uid.
    #[test]
    fn new_inode_does_not_overwrite_next_inode() {
        let (path, mut s) = store("neighbour");
        s.write_inode(4, &inode(0o040775, 1000));
        s.write_inode(3, &inode(0o100664, 1000));

        let next = s.read_inode(4);
        assert_eq!(next.mode, 0o040775, "next inode's mode was overwritten");
        assert_eq!(next.uid, 1000, "next inode's uid was overwritten");
        assert_eq!(s.read_inode(3).mode, 0o100664);
        let _ = std::fs::remove_file(path);
    }

    //Fill every slot of one table block, including slot 15 (the last one,
    //which used to spill 8 bytes past the 4 KiB block), and check that each
    //inode reads back exactly as written. Slots are written in REVERSE order:
    //written in increasing order, each write would re-stamp the bytes the
    //previous write spilled onto, hiding the bug.
    #[test]
    fn every_slot_in_a_block_round_trips() {
        let (path, mut s) = store("allslots");
        let per_block = BLOCK_SIZE / INODE_SIZE;
        for n in (0..per_block).rev() {
            s.write_inode(n, &inode(0o100000 | n as u32, 1000 + n as u32));
        }
        for n in 0..per_block {
            let got = s.read_inode(n);
            assert_eq!(got.mode, 0o100000 | n as u32, "slot {n} mode");
            assert_eq!(got.uid, 1000 + n as u32, "slot {n} uid");
        }
        let _ = std::fs::remove_file(path);
    }
}
