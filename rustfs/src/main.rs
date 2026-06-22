use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fuser::{FileAttr, MountOption, ReplyAttr, Request};
use fuser::{FileType, Filesystem};
use libc::{EFBIG, ENOENT, ENOSPC};
use libfs::bitmap::Bitmap;
use libfs::inode::InodeStore;
use libfs::layout::{BLOCK_SIZE, DirEntryHeader, FT_DIRECTORY, FT_REGULAR};
use libfs::{
    dir::DirStore,
    disk::Disk,
    layout::{Inode, MAGIC, Superblock},
};

const TTL: Duration = Duration::from_secs(1);

pub struct RustFS {
    disk: Arc<Mutex<Disk>>,
    superblock: Superblock,
}

impl RustFS {
    pub fn new(image_path: &str) -> Self {
        let mut disk = Disk::open(image_path).unwrap();

        //read superblock from block 0
        let sb_block = disk.read_block(0).unwrap();
        let superblock: Superblock =
            unsafe { std::ptr::read(sb_block.as_ptr() as *const Superblock) };

        assert_eq!(superblock.magic, MAGIC, "Not a rustfs filesystem!");

        RustFS {
            disk: Arc::new(Mutex::new(disk)),
            superblock,
        }
    }

    //helper: convert Inode to FUSE FileAttr
    fn inode_to_attr(&self, ino: u64, inode: &Inode) -> FileAttr {
        let kind = if inode.mode & 0o170000 == 0o040000 {
            FileType::Directory
        } else {
            FileType::RegularFile
        };

        FileAttr {
            ino,
            size: inode.size,
            blocks: (inode.size + 511) / 512,
            atime: UNIX_EPOCH + Duration::from_secs(inode.atime),
            mtime: UNIX_EPOCH + Duration::from_secs(inode.mtime),
            ctime: UNIX_EPOCH + Duration::from_secs(inode.ctime),
            crtime: UNIX_EPOCH,
            kind,
            perm: (inode.mode & 0o777) as u16,
            nlink: inode.hard_links,
            uid: inode.uid,
            gid: inode.gid,
            rdev: 0,
            blksize: BLOCK_SIZE as u32,
            flags: 0,
        }
    }
}

fn main() {
    let image_path = std::env::args()
        .nth(1)
        .expect("Usage: rustfs <image> <mountpoint>");
    let mountpoint = std::env::args()
        .nth(2)
        .expect("Usage: rustfs <image> <mountpoint>");

    let filesystem = RustFS::new(&image_path);

    let options = vec![
        MountOption::RW,
        MountOption::FSName("rustfs".to_string()),
        MountOption::AutoUnmount,
    ];

    fuser::mount2(filesystem, &mountpoint, &options).unwrap();
}

impl Filesystem for RustFS {
    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        let real_ino = if ino == 1 { 2 } else { ino };
        let mut inode_store = InodeStore::new(self.disk.clone(), self.superblock.inode_table_block);
        let inode = inode_store.read_inode(real_ino);

        if inode.mode == 0 {
            reply.error(ENOENT);
            return;
        }
        let attr = self.inode_to_attr(ino, &inode);
        reply.attr(&TTL, &attr);
    }

    fn lookup(
        &mut self,
        _req: &Request,
        parent: u64,
        name: &std::ffi::OsStr,
        reply: fuser::ReplyEntry,
    ) {
        let real_parent = if parent == 1 { 2 } else { parent };
        let name_str = name.to_str().unwrap();
        let mut dir_store = DirStore::new(
            self.disk.clone(),
            InodeStore::new(self.disk.clone(), self.superblock.inode_table_block),
        );

        match dir_store.look_up(real_parent, name_str.to_string()) {
            Some(inode_num) => {
                let mut inode_store =
                    InodeStore::new(self.disk.clone(), self.superblock.inode_table_block);
                let inode = inode_store.read_inode(inode_num as u64);
                let attr = self.inode_to_attr(inode_num as u64, &inode);
                reply.entry(&TTL, &attr, 0);
            }
            None => reply.error(ENOENT),
        }
    }

    fn readdir(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: fuser::ReplyDirectory,
    ) {
        let real_ino = if ino == 1 { 2 } else { ino };
        let mut dir_store = DirStore::new(
            self.disk.clone(),
            InodeStore::new(self.disk.clone(), self.superblock.inode_table_block),
        );

        let entries = dir_store.list(real_ino);

        for (i, (inode_num, name)) in entries.iter().enumerate().skip(offset as usize) {
            let mut inode_store =
                InodeStore::new(self.disk.clone(), self.superblock.inode_table_block);

            let inode = inode_store.read_inode(*inode_num as u64);
            let kind = if inode.mode & 0o170000 == 0o040000 {
                FileType::Directory
            } else {
                FileType::RegularFile
            };

            let full = reply.add(*inode_num as u64, (i + 1) as i64, kind, name);
            if full {
                break;
            }
        }
        reply.ok();
    }

    fn create(
        &mut self,
        _req: &Request,
        parent: u64,
        name: &std::ffi::OsStr,
        mode: u32,
        _umask: u32,
        _flags: i32,
        reply: fuser::ReplyCreate,
    ) {
        let real_parent = if parent == 1 { 2 } else { parent };
        let name_str = name.to_str().unwrap();

        //find an open bit i.e. 0 bit
        let inode_bitmap_block = self.superblock.inode_bitmap_block;
        let bitmap_data = self
            .disk
            .lock()
            .unwrap()
            .read_block(inode_bitmap_block)
            .unwrap();

        let mut bitmap = Bitmap::from_block(bitmap_data);

        let inode_num = match bitmap.alloc() {
            Some(n) => n,
            None => {
                reply.error(libc::ENOSPC);
                return;
            }
        };

        let update_bitmap = bitmap.to_block();
        self.disk
            .lock()
            .unwrap()
            .write_block(inode_bitmap_block, &update_bitmap)
            .unwrap();

        //update superblock free inode count
        self.superblock.free_inodes -= 1;
        let mut sb_block = [0u8; 4096];

        unsafe {
            std::ptr::write(sb_block.as_mut_ptr() as *mut Superblock, self.superblock);
        }

        self.disk.lock().unwrap().write_block(0, &sb_block).unwrap();

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        //after finding the inode number initialize the inode struct
        let new_inode = Inode {
            mode,
            uid: _req.uid(),
            gid: _req.gid(),
            hard_links: 1,
            size: 0,
            atime: now,
            mtime: now,
            ctime: now,
            direct: [0u64; 12],
            indirect: 0,
            double_indirect: 0,
            _padding: [0u8; 96],
        };

        let mut inode_store = InodeStore::new(self.disk.clone(), self.superblock.inode_table_block);
        inode_store.write_inode(inode_num, &new_inode);

        //add directory entry in parent
        let mut dir_store = DirStore::new(
            self.disk.clone(),
            InodeStore::new(self.disk.clone(), self.superblock.inode_table_block),
        );
        dir_store.add_entry(real_parent, name_str, inode_num as u32, FT_REGULAR);

        //reply to fuse with the new file attributes
        let attr = self.inode_to_attr(inode_num, &new_inode);
        reply.created(&TTL, &attr, 0, 0, 0);
    }

    fn write(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: fuser::ReplyWrite,
    ) {
        //read the current inode
        let mut inode_store = InodeStore::new(self.disk.clone(), self.superblock.inode_table_block);
        let mut inode = inode_store.read_inode(ino);

        //figure out which block the data goes
        let block_index = offset as usize / BLOCK_SIZE as usize;
        let bytes_offset_in_block = offset as usize % BLOCK_SIZE as usize;

        if block_index >= 12 {
            reply.error(EFBIG);
            return;
        }

        //allocate data block if none exists
        let block_num = if inode.direct[block_index] == 0 {
            let data_bitmap_block = self.superblock.data_bitmap_block;
            let bitmap_data = self
                .disk
                .lock()
                .unwrap()
                .read_block(data_bitmap_block)
                .unwrap();
            let mut bitmap = Bitmap::from_block(bitmap_data);

            let new_block_index = match bitmap.alloc() {
                Some(n) => n,
                None => {
                    reply.error(ENOSPC);
                    return;
                }
            };

            self.disk
                .lock()
                .unwrap()
                .write_block(data_bitmap_block, &bitmap.to_block())
                .unwrap();

            //convert bitmap index to actual disk block numer
            let actual_block = self.superblock.data_start_block + new_block_index;
            inode.direct[block_index] = actual_block;
            actual_block
        } else {
            inode.direct[block_index]
        };

        //the block on which the data is to be written
        let mut block = self.disk.lock().unwrap().read_block(block_num).unwrap();

        let end = bytes_offset_in_block + data.len();
        block[bytes_offset_in_block..end].copy_from_slice(data);

        //write the actual bytes on the block
        self.disk
            .lock()
            .unwrap()
            .write_block(block_num, &block)
            .unwrap();

        //update the inode metadata
        let new_size = (offset as u64 + data.len() as u64).max(inode.size);
        inode.size = new_size;
        inode.mtime = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        inode_store.write_inode(ino, &inode);

        //reply to fuse with byte written
        reply.written(data.len() as u32);
    }

    fn read(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: fuser::ReplyData,
    ) {
        let mut inode_store = InodeStore::new(self.disk.clone(), self.superblock.inode_table_block);
        let inode = inode_store.read_inode(ino);

        if offset as u64 >= inode.size {
            reply.data(&[]);
            return;
        }

        let block_index = offset as usize / BLOCK_SIZE as usize;
        let byte_offset_in_block = offset as usize % BLOCK_SIZE as usize;

        if block_index >= 12 || inode.direct[block_index] == 0 {
            reply.data(&[]);
            return;
        }

        let block_num = inode.direct[block_index];

        let block = self.disk.lock().unwrap().read_block(block_num).unwrap();

        //how many bytes can we read?? //requested size, remaining file size, remaining space in the block
        let remaining_in_file = (inode.size - offset as u64) as usize;
        let remaining_in_block = BLOCK_SIZE as usize - byte_offset_in_block;
        let bytes_to_read = (size as usize)
            .min(remaining_in_block)
            .min(remaining_in_file);

        let data = &block[byte_offset_in_block..byte_offset_in_block + bytes_to_read];
        reply.data(data);
    }

    fn mkdir(
        &mut self,
        _req: &Request,
        parent: u64,
        name: &std::ffi::OsStr,
        mode: u32,
        _umask: u32,
        reply: fuser::ReplyEntry,
    ) {
        let mut inode_store = InodeStore::new(self.disk.clone(), self.superblock.inode_table_block);

        let inode_bitmap_block = self.superblock.inode_bitmap_block;
        let bitmap_data = self
            .disk
            .lock()
            .unwrap()
            .read_block(inode_bitmap_block)
            .unwrap();

        let mut bitmap = Bitmap::from_block(bitmap_data);

        let inode_num = match bitmap.alloc() {
            Some(n) => n,
            None => {
                reply.error(ENOSPC);
                return;
            }
        };

        //write back the updated bitmap
        let updated_bitmap = bitmap.to_block();
        self.disk
            .lock()
            .unwrap()
            .write_block(inode_bitmap_block, &updated_bitmap)
            .unwrap();

        self.superblock.free_inodes -= 1;
        let mut sb_block = [0u8; 4096];

        unsafe {
            std::ptr::write(sb_block.as_mut_ptr() as *mut Superblock, self.superblock);
        }

        self.disk.lock().unwrap().write_block(0, &sb_block).unwrap();

        //allocate data block for "." and ".." entries
        let data_bitmap_block = self.superblock.data_bitmap_block;
        let data_bitmap_data = self
            .disk
            .lock()
            .unwrap()
            .read_block(data_bitmap_block)
            .unwrap();
        let mut data_bitmap = Bitmap::from_block(data_bitmap_data);

        let new_block_index = match data_bitmap.alloc() {
            Some(n) => n,
            None => {
                reply.error(ENOSPC);
                return;
            }
        };

        self.disk
            .lock()
            .unwrap()
            .write_block(data_bitmap_block, &data_bitmap.to_block())
            .unwrap();

        let actual_block = self.superblock.data_start_block + new_block_index;

        //write "." and ".." into that block
        let real_parent = if parent == 1 { 2 } else { parent };

        let mut dir_block = [0u8; 4096];

        //"." -> points to itself (the new directory)
        let dot_healer = DirEntryHeader {
            inode: inode_num as u32,
            rec_len: 12,
            name_len: 1,
            file_type: FT_DIRECTORY,
        };

        unsafe {
            std::ptr::write(dir_block.as_mut_ptr() as *mut DirEntryHeader, dot_healer);
        }
        dir_block[8] = b'.';

        //".." -> points to parent
        let dotdot_header = DirEntryHeader {
            inode: real_parent as u32,
            rec_len: (BLOCK_SIZE - 12) as u16,
            name_len: 2,
            file_type: FT_DIRECTORY,
        };

        unsafe {
            std::ptr::write(
                dir_block[12..].as_mut_ptr() as *mut DirEntryHeader,
                dotdot_header,
            );
        }
        dir_block[20] = b'.';
        dir_block[21] = b'.';

        self.disk
            .lock()
            .unwrap()
            .write_block(actual_block, &dir_block)
            .unwrap();

        //initialize and write the new directory's inode
        let now = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let mut direct = [0u64; 12];
        direct[0] = actual_block;

        let new_inode = Inode {
            mode: mode | 0o040000,
            uid: _req.uid(),
            gid: _req.gid(),
            hard_links: 2, //"." and ".."
            size: BLOCK_SIZE,
            atime: now,
            ctime: now,
            mtime: now,
            direct,
            indirect: 0,
            double_indirect: 0,
            _padding: [0u8; 96],
        };

        inode_store.write_inode(inode_num, &new_inode);

        //add entry in parent + increment parent's hard links
        let mut dir_store = DirStore::new(
            self.disk.clone(),
            InodeStore::new(self.disk.clone(), self.superblock.inode_table_block),
        );
        let name_str = name.to_str().unwrap();
        dir_store.add_entry(real_parent, name_str, inode_num as u32, FT_DIRECTORY);

        //increment parent's hard links (new ".." points to it)
        let mut parent_inode = inode_store.read_inode(real_parent);
        parent_inode.hard_links += 1;
        inode_store.write_inode(real_parent, &parent_inode);

        let attr = self.inode_to_attr(inode_num, &new_inode);
        reply.entry(&TTL, &attr, 0);
    }

    fn unlink(
        &mut self,
        _req: &Request,
        parent: u64,
        name: &std::ffi::OsStr,
        reply: fuser::ReplyEmpty,
    ) {
        let real_parent = if parent == 1 { 2 } else { parent };
        let name_str = name.to_str().unwrap();

        let mut dir_store = DirStore::new(
            self.disk.clone(),
            InodeStore::new(self.disk.clone(), self.superblock.inode_table_block),
        );

        //find the inode numer using lookup
        let inode_num = match dir_store.look_up(real_parent, name_str.to_string()) {
            Some(n) => n as u64,
            None => {
                reply.error(ENOENT);
                return;
            }
        };
        //read that inode
        let mut inode_store = InodeStore::new(self.disk.clone(), self.superblock.inode_table_block);

        let mut inode = inode_store.read_inode(inode_num);

        //decrement the hard links
        inode.hard_links -= 1;

        //if hard_links == 0:
        //      free the data blocks
        //      free the inode
        if inode.hard_links == 0 {
            for i in 0..12 {
                if inode.direct[i] != 0 {
                    let block_offset = inode.direct[i] - self.superblock.data_start_block;
                    let data_bitmap_block = self.superblock.data_bitmap_block;
                    let bitmap_data = self
                        .disk
                        .lock()
                        .unwrap()
                        .read_block(data_bitmap_block)
                        .unwrap();
                    let mut bitmap = Bitmap::from_block(bitmap_data);
                    bitmap.free(block_offset);
                    self.disk
                        .lock()
                        .unwrap()
                        .write_block(data_bitmap_block, &bitmap.to_block())
                        .unwrap();
                }
            }

            //free the inode
            let inode_bitmap_block = self.superblock.inode_bitmap_block;
            let bitmap_data = self.disk.lock().unwrap().read_block(inode_num).unwrap();
            let mut bitmap = Bitmap::from_block(bitmap_data);
            bitmap.free(inode_num);
            self.disk
                .lock()
                .unwrap()
                .write_block(inode_bitmap_block, &bitmap.to_block())
                .unwrap();
        } else {
            //hard links still > 0, just write the update inode back
            inode_store.write_inode(inode_num, &inode);
        }
        //remove the directory entry
        match dir_store.remove_entry(real_parent, name_str) {
            Ok(()) => reply.ok(),
            Err(_) => reply.error(ENOENT),
        }
    }

    fn setattr(
        &mut self,
        _req: &Request,
        ino: u64,
        _mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        _size: Option<u64>,
        atime: Option<fuser::TimeOrNow>,
        mtime: Option<fuser::TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<u64>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        let mut inode_store = InodeStore::new(self.disk.clone(), self.superblock.inode_table_block);
        let mut inode = inode_store.read_inode(ino);

        if let Some(atime) = atime {
            inode.atime = match atime {
                fuser::TimeOrNow::SpecificTime(t) => {
                    t.duration_since(UNIX_EPOCH).unwrap().as_secs()
                }
                fuser::TimeOrNow::Now => std::time::SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
            };
        }

        if let Some(mtime) = mtime {
            inode.mtime = match mtime {
                fuser::TimeOrNow::SpecificTime(t) => {
                    t.duration_since(UNIX_EPOCH).unwrap().as_secs()
                }
                fuser::TimeOrNow::Now => std::time::SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
            }
        }

        inode_store.write_inode(ino, &inode);
        let attr = self.inode_to_attr(ino, &inode);
        reply.attr(&TTL, &attr);
    }
}
