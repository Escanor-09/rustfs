use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fuser::{FileAttr, MountOption, ReplyAttr, Request};
use fuser::{FileType, Filesystem};
use libc::ENOENT;
use libfs::bitmap::Bitmap;
use libfs::inode::InodeStore;
use libfs::layout::{BLOCK_SIZE, FT_REGULAR};
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
