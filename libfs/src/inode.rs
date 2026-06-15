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
