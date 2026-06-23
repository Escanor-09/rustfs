use crate::disk::Disk;
use crate::inode::InodeStore;
use crate::layout::BLOCK_SIZE;
use crate::layout::DirEntryHeader;
use std::sync::Arc; //Automatically Reference counted
use std::sync::Mutex; //Mututal Exclusion

pub struct DirStore {
    disk: Arc<Mutex<Disk>>,
    inode_store: InodeStore,
}

impl DirStore {
    pub fn new(disk: Arc<Mutex<Disk>>, inode_store: InodeStore) -> Self {
        DirStore { disk, inode_store }
    }

    //looking up the directory entries with the name of the file and locating its inode and returning it
    pub fn look_up(&mut self, dir_inode_num: u64, name: String) -> Option<u32> {
        //read directory entry inode
        let inode = self.inode_store.read_inode(dir_inode_num);
        let block_num = inode.direct[0];

        //read data blcok from direct[0]
        let block = self.disk.lock().unwrap().read_block(block_num).unwrap();

        let mut offset = 0usize;
        //walk through the entries
        while offset < BLOCK_SIZE as usize {
            //this is interpreting it as DirEntryHeader
            let header =
                unsafe { std::ptr::read(block[offset..].as_ptr() as *const DirEntryHeader) };

            //if inode is 0 this entry is empty
            if header.rec_len == 0 {
                break;
            }

            if header.inode != 0 {
                //read the name
                let name_start = offset + 8;
                let name_end = name_start + header.name_len as usize;
                let fname = String::from_utf8_lossy(&block[name_start..name_end]).to_string();

                if fname == name {
                    return Some(header.inode);
                }
            }
            offset += header.rec_len as usize;
        }
        None
    }

    //adding an entry in the directory
    pub fn add_entry(&mut self, dir_inode_num: u64, name: &str, inode_num: u32, file_type: u8) {
        //read the directory inode
        let inode = self.inode_store.read_inode(dir_inode_num);
        let block_num = inode.direct[0];

        //read the data block
        let mut block = self.disk.lock().unwrap().read_block(block_num).unwrap();

        let mut offset = 0usize;
        let actual_size = ((8 + name.len()) + 3) & !3;

        while offset < BLOCK_SIZE as usize {
            let header =
                unsafe { std::ptr::read(block[offset..].as_ptr() as *const DirEntryHeader) };

            //if the deleted slot has enough space then reuse it
            if header.inode == 0 && header.rec_len as usize >= actual_size {
                let leftover = header.rec_len as usize - actual_size;

                let new_record_len = if leftover >= 8 {
                    actual_size
                } else {
                    actual_size + leftover
                };

                //new entry gets actual size
                let new_header = DirEntryHeader {
                    inode: inode_num,
                    rec_len: new_record_len as u16, // keep the same as rec_len
                    name_len: name.len() as u8,
                    file_type,
                };

                unsafe {
                    std::ptr::write(
                        block[offset..].as_mut_ptr() as *mut DirEntryHeader,
                        new_header,
                    );
                }

                block[offset + 8..offset + 8 + name.len()].copy_from_slice(name.as_bytes());

                //writing leftover as empty slot
                if leftover >= 8 {
                    let leftover_header = DirEntryHeader {
                        inode: 0,
                        rec_len: leftover as u16,
                        name_len: 0,
                        file_type: 0,
                    };

                    unsafe {
                        std::ptr::write(
                            block[offset + actual_size..].as_mut_ptr() as *mut DirEntryHeader,
                            leftover_header,
                        );
                    }
                }

                self.disk
                    .lock()
                    .unwrap()
                    .write_block(block_num, &block)
                    .unwrap();
                return;
            }

            //if it is the last entry-shrink it and append new entry after it
            if offset + header.rec_len as usize == BLOCK_SIZE as usize {
                let last_actual = ((8 + header.name_len as usize) + 3) & !3;
                let remaining = header.rec_len as usize - last_actual;

                //shrink last entry rec_len to its actual size
                let shrunk_header = DirEntryHeader {
                    inode: header.inode,
                    rec_len: last_actual as u16,
                    name_len: header.name_len,
                    file_type: header.file_type,
                };

                unsafe {
                    std::ptr::write(
                        block[offset..].as_mut_ptr() as *mut DirEntryHeader,
                        shrunk_header,
                    );
                }

                //write new entry after the shrunk header
                let new_offset = offset + last_actual;
                let new_header = DirEntryHeader {
                    inode: inode_num,
                    rec_len: remaining as u16,
                    name_len: name.len() as u8,
                    file_type,
                };

                unsafe {
                    std::ptr::write(
                        block[new_offset..].as_mut_ptr() as *mut DirEntryHeader,
                        new_header,
                    );
                }

                block[new_offset + 8..new_offset + 8 + name.len()].copy_from_slice(name.as_bytes());

                self.disk
                    .lock()
                    .unwrap()
                    .write_block(block_num, &block)
                    .unwrap();
                return;
            }
            offset += header.rec_len as usize;
        }
    }

    //removing entry from the directory
    pub fn remove_entry(&mut self, dir_inode_num: u64, name: &str) -> Result<(), String> {
        //read directory inode
        let inode = self.inode_store.read_inode(dir_inode_num);

        //read data block
        let block_num = inode.direct[0];
        let mut block = self.disk.lock().unwrap().read_block(block_num).unwrap();

        let mut offset = 0usize;

        while offset < BLOCK_SIZE as usize {
            let header =
                unsafe { std::ptr::read(block[offset..].as_mut_ptr() as *const DirEntryHeader) };

            if header.rec_len == 0 {
                break;
            }

            if header.inode != 0 {
                let name_start = offset + 8;
                let name_end = name_start + header.name_len as usize;
                let fname = String::from_utf8_lossy(&block[name_start..name_end]).to_string();

                if fname == name {
                    let new_header = DirEntryHeader {
                        inode: 0,
                        rec_len: header.rec_len,
                        name_len: header.name_len,
                        file_type: header.file_type,
                    };

                    unsafe {
                        std::ptr::write(
                            block[offset..].as_mut_ptr() as *mut DirEntryHeader,
                            new_header,
                        );
                    };

                    self.disk
                        .lock()
                        .unwrap()
                        .write_block(block_num, &block)
                        .unwrap();
                    return Ok(());
                }
            }
            offset += header.rec_len as usize;
        }
        Err("ENONENT: no such file or directory".to_string())
    }

    //listing out all the files in the directories
    pub fn list(&mut self, dir_inode_num: u64) -> Vec<(u32, String)> {
        //read the directory inode
        let inode = self.inode_store.read_inode(dir_inode_num);

        //read the data block
        let block_num = inode.direct[0];
        let block = self.disk.lock().unwrap().read_block(block_num).unwrap();

        //walk through the entries
        let mut entries = Vec::new();
        let mut offset = 0usize;

        while offset < BLOCK_SIZE as usize {
            //read header
            let header: DirEntryHeader =
                unsafe { std::ptr::read(block[offset..].as_ptr() as *const DirEntryHeader) };

            //if inode is 0 this entry is empty
            if header.rec_len == 0 {
                break;
            }

            if header.inode != 0 {
                //read name bytes after header 8 bytes
                let name_start = offset + 8;
                let name_end = name_start + header.name_len as usize;
                let name = String::from_utf8_lossy(&block[name_start..name_end]).to_string();

                //push to entries
                entries.push((header.inode, name));
            }
            //jump by rec_len
            offset += header.rec_len as usize;
        }
        entries
    }

    pub fn prepare_add_entry(
        &mut self,
        dir_inode_num: u64,
        name: &str,
        inode_num: u32,
        file_type: u8,
    ) -> (u64, [u8; 4096]) {
        let inode = self.inode_store.read_inode(dir_inode_num);
        let block_num = inode.direct[0];
        let mut block = self.disk.lock().unwrap().read_block(block_num).unwrap();

        let actual_size = ((8 + name.len()) + 3) & !3;
        let mut offset = 0usize;

        while offset < BLOCK_SIZE as usize {
            let header =
                unsafe { std::ptr::read(block[offset..].as_ptr() as *const DirEntryHeader) };

            if header.inode == 0 && header.rec_len as usize >= actual_size {
                let leftover = header.rec_len as usize - actual_size;
                let new_rec_len = if leftover >= 8 {
                    actual_size
                } else {
                    actual_size + leftover
                };

                let new_header = DirEntryHeader {
                    inode: inode_num,
                    rec_len: new_rec_len as u16,
                    name_len: name.len() as u8,
                    file_type,
                };

                unsafe {
                    std::ptr::write(
                        block[offset..].as_mut_ptr() as *mut DirEntryHeader,
                        new_header,
                    );
                }

                block[offset + 8..offset + 8 + name.len()].copy_from_slice(name.as_bytes());

                if leftover >= 8 {
                    let leftover_header = DirEntryHeader {
                        inode: 0,
                        rec_len: leftover as u16,
                        name_len: 0,
                        file_type: 0,
                    };
                    unsafe {
                        std::ptr::write(
                            block[offset + actual_size..].as_mut_ptr() as *mut DirEntryHeader,
                            leftover_header,
                        );
                    }
                }
                return (block_num, block);
            }

            if offset + header.rec_len as usize == BLOCK_SIZE as usize {
                let last_actual = ((8 + header.name_len as usize) + 3) & !3;
                let remaining = header.rec_len as usize - last_actual;

                let shrunk_header = DirEntryHeader {
                    inode: header.inode,
                    rec_len: last_actual as u16,
                    name_len: header.name_len,
                    file_type: header.file_type,
                };
                unsafe {
                    std::ptr::write(
                        block[offset..].as_mut_ptr() as *mut DirEntryHeader,
                        shrunk_header,
                    );
                }

                let new_offset = offset + last_actual;
                let new_header = DirEntryHeader {
                    inode: inode_num,
                    rec_len: remaining as u16,
                    name_len: name.len() as u8,
                    file_type,
                };

                unsafe {
                    std::ptr::write(
                        block[new_offset..].as_mut_ptr() as *mut DirEntryHeader,
                        new_header,
                    );
                }
                block[new_offset + 8..new_offset + 8 + name.len()].copy_from_slice(name.as_bytes());
                return (block_num, block);
            }
            offset += header.rec_len as usize;
        }
        panic!("directory block full, no space for new entry");
    }
}
