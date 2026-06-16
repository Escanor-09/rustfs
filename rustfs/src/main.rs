use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use fuser::{FileAttr, MountOption, ReplyAttr, Request};
use fuser::{FileType, Filesystem};
use libc::ENOENT;
use libfs::inode::InodeStore;
use libfs::layout::BLOCK_SIZE;
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
}
