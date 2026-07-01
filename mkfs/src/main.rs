use libfs::{disk::Disk, layout::*};

fn main() {
    //parse: path and size
    let path = "myfs.img";
    let size_mb: u64 = 100;
    let size_bytes = size_mb * 1024 * 1024;
    let total_blocks = size_bytes / BLOCK_SIZE;

    //create blank image
    let mut disk = Disk::create(path, size_bytes).unwrap();
    println!("Created blank disk image: {}", path);

    //create layout
    let inode_bitmap_block: u64 = 1;
    let data_bitmap_block: u64 = 2;
    let inode_table_block: u64 = 3;
    let inode_table_blocks: u64 = 128;
    let journal_start: u64 = inode_table_block + inode_table_blocks;
    let journal_blocks: u64 = 8;
    let data_start: u64 = journal_start + journal_blocks;
    let total_inodes: u64 = inode_table_blocks * (BLOCK_SIZE / INODE_SIZE);
    let total_data_blocks: u64 = total_blocks - data_start;

    //write super block
    let superblock = Superblock {
        magic: MAGIC,
        block_size: BLOCK_SIZE as u32,
        total_blocks,
        total_inodes,
        free_blocks: total_data_blocks - 1, //-1 for root dir block
        free_inodes: total_inodes - 3,      //-1 for root inode
        inode_bitmap_block,
        data_bitmap_block,
        inode_table_block,
        journal_start_block: journal_start,
        data_start_block: data_start,
        _padding: [0u8; 3996],
    };

    let mut sb_block = [0u8; 4096];
    //serialize super block to block 0
    unsafe {
        std::ptr::write(sb_block.as_mut_ptr() as *mut Superblock, superblock);
    }

    disk.write_block(0, &sb_block).unwrap();
    println!("Wrote superblock");

    //write empty bitmaps;
    let mut inode_bitmap_data = [0u8; 4096];
    inode_bitmap_data[0] = 0b00000111; //bit 0,1,2 set
    disk.write_block(inode_bitmap_block, &inode_bitmap_data)
        .unwrap();
    println!("Wrote inode bitmap");

    //write block bitmap
    let mut block_bitmap_data = [0u8; 4096];
    block_bitmap_data[0] = 0b00000001;
    disk.write_block(data_bitmap_block, &block_bitmap_data)
        .unwrap();
    println!("Wrote block bitmap");

    //write empty inode table
    let empty_block = [0u8; 4096];
    for i in 0..inode_table_blocks {
        disk.write_block(inode_table_block + i, &empty_block)
            .unwrap();
    }
    println!("Wrote Inode Table");

    //write root inode
    let root_inode = Inode {
        mode: 0o040755,
        uid: 0,
        gid: 0,
        hard_links: 2, //"." and ".."
        size: BLOCK_SIZE,
        atime: 0,
        mtime: 0,
        ctime: 0,
        direct: [data_start, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        indirect: 0,
        double_indirect: 0,
        _padding: [0u8; 96],
    };

    let mut inode_block = disk.read_block(inode_table_block).unwrap();
    unsafe {
        std::ptr::write(inode_block[512..].as_mut_ptr() as *mut Inode, root_inode);
    }

    disk.write_block(inode_table_block, &inode_block).unwrap();
    println!("Wrote root inode");

    //write root directory data block
    let mut dir_block = [0u8; 4096];

    //"." entry - points to itself (inode 2)
    let dot_header = DirEntryHeader {
        inode: ROOT_INODE,
        rec_len: 12,
        name_len: 1,
        file_type: FT_DIRECTORY,
    };

    unsafe {
        std::ptr::write(dir_block.as_mut_ptr() as *mut DirEntryHeader, dot_header);
    }

    dir_block[8] = b'.';

    // ".." entry- parent is also root (inode 2), stretches to the end of the block
    let dotdot_header = DirEntryHeader {
        inode: ROOT_INODE,
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

    disk.write_block(data_start, &dir_block).unwrap();
    println!("Wrote root directory block");
    disk.flush_all();
    println!("mkfs complete! filesystem ready at {}", path);
}
