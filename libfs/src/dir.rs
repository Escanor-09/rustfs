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
    //returns None (and writes nothing) if the directory block has no room for `name`
    pub fn add_entry(
        &mut self,
        dir_inode_num: u64,
        name: &str,
        inode_num: u32,
        file_type: u8,
    ) -> Option<()> {
        let (block_num, block) =
            self.prepare_add_entry(dir_inode_num, name, inode_num, file_type)?;
        self.disk
            .lock()
            .unwrap()
            .write_block(block_num, &block)
            .unwrap();
        Some(())
    }

    //true if `name` would fit in the directory block; nothing is written
    pub fn has_space_for(&mut self, dir_inode_num: u64, name: &str) -> bool {
        self.prepare_add_entry(dir_inode_num, name, 0, 0).is_some()
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
    ) -> Option<(u64, [u8; 4096])> {
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
                return Some((block_num, block));
            }

            if offset + header.rec_len as usize == BLOCK_SIZE as usize {
                let last_actual = ((8 + header.name_len as usize) + 3) & !3;
                let remaining = header.rec_len as usize - last_actual;

                //the new entry must fit in the space after the last entry;
                //otherwise the directory block is full
                if remaining < actual_size {
                    return None;
                }

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
                return Some((block_num, block));
            }
            offset += header.rec_len as usize;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{FT_REGULAR, Inode};

    const DIR_INODE_NUM: u64 = 5;
    const DIR_DATA_BLOCK: u64 = 1;

    //a tiny image: block 0 = inode table holding one directory inode,
    //block 1 = that directory's data block, starting as one empty slot
    fn setup(name: &str) -> (std::path::PathBuf, DirStore) {
        let path = std::env::temp_dir().join(format!(
            "rustfs-dirtest-{}-{}.img",
            name,
            std::process::id()
        ));
        let disk = Arc::new(Mutex::new(
            Disk::create(path.to_str().unwrap(), 4 * BLOCK_SIZE).unwrap(),
        ));

        let dir_inode = Inode {
            mode: 0o040755,
            uid: 0,
            gid: 0,
            size: BLOCK_SIZE,
            ctime: 0,
            atime: 0,
            mtime: 0,
            hard_links: 2,
            direct: [DIR_DATA_BLOCK, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            indirect: 0,
            double_indirect: 0,
            _padding: [0u8; 96],
        };
        InodeStore::new(disk.clone(), 0).write_inode(DIR_INODE_NUM, &dir_inode);

        let mut block = [0u8; BLOCK_SIZE as usize];
        let empty = DirEntryHeader {
            inode: 0,
            rec_len: BLOCK_SIZE as u16,
            name_len: 0,
            file_type: 0,
        };
        unsafe { std::ptr::write(block.as_mut_ptr() as *mut DirEntryHeader, empty) };
        disk.lock().unwrap().write_block(DIR_DATA_BLOCK, &block).unwrap();

        let store = DirStore::new(disk.clone(), InodeStore::new(disk, 0));
        (path, store)
    }

    //names f0..f340 each take 12 bytes, so exactly 341 fit in 4096 bytes
    //(this is the case that used to panic at dir.rs:351 on the 342nd add)
    fn fill(store: &mut DirStore) -> usize {
        let mut n = 0;
        while store
            .add_entry(DIR_INODE_NUM, &format!("f{n}"), 100 + n as u32, FT_REGULAR)
            .is_some()
        {
            n += 1;
            assert!(n <= 400, "directory never reported full");
        }
        n
    }

    #[test]
    fn full_directory_returns_none_instead_of_panicking() {
        let (path, mut store) = setup("full");
        assert_eq!(fill(&mut store), 341);

        let before = store.disk.lock().unwrap().read_block(DIR_DATA_BLOCK).unwrap();
        assert!(!store.has_space_for(DIR_INODE_NUM, "x"));
        assert!(store.prepare_add_entry(DIR_INODE_NUM, "x", 999, FT_REGULAR).is_none());
        assert!(store.add_entry(DIR_INODE_NUM, "x", 999, FT_REGULAR).is_none());
        let after = store.disk.lock().unwrap().read_block(DIR_DATA_BLOCK).unwrap();
        assert_eq!(before, after, "a failed add must not change the block");

        //every existing entry is still readable
        assert_eq!(store.list(DIR_INODE_NUM).len(), 341);
        assert_eq!(store.look_up(DIR_INODE_NUM, "f340".into()), Some(440));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn removed_slot_is_reused_when_full() {
        let (path, mut store) = setup("reuse");
        fill(&mut store);

        store.remove_entry(DIR_INODE_NUM, "f100").unwrap();
        assert!(store.has_space_for(DIR_INODE_NUM, "new"));
        assert!(store.add_entry(DIR_INODE_NUM, "new", 999, FT_REGULAR).is_some());
        assert_eq!(store.look_up(DIR_INODE_NUM, "new".into()), Some(999));
        assert!(!store.has_space_for(DIR_INODE_NUM, "x"));

        let _ = std::fs::remove_file(path);
    }
}
