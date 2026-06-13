pub const BLOCK_SIZE: u64 = 4096;
pub const MAGIC: u32 = 0x52465301;
pub const INODE_SIZE: u64 = 256;
pub const ROOT_INODE: u32 = 2;

//Flie types for DirEntry.file_tye
pub const FT_REGULAR: u8 = 1;
pub const FT_DIRECTORY: u8 = 2;
pub const FT_SYMLINK: u8 = 7;

//Struct of the Superblock which contains the information of all the blocks
pub struct Superblock {
    pub magic: u32,
    pub block_Size: u32,
    pub total_blocks: u64,
    pub total_inodes: u64,
    pub free_blocks: u64,
    pub free_inodes: u64,
    pub inode_bitmap_block: u64,
    pub data_bitmap_block: u64,
    pub inode_table_block: u64,
    pub journal_start_block: u64,
    pub data_Start_block: u64,
    pub _padding: [u8; 3996],
}

//Inode Table Struct
pub struct Inode {
    pub mode: u32,            //file type + persmissions
    pub uid: u32,             //owner user id
    pub gid: u32,             //owner group id
    pub size: u64,            //filesize in bytes
    pub ctime: u64,           //last status change time
    pub atime: u64,           //last access time
    pub mtime: u64,           //last modified time
    pub hard_links: u32,      //number of directory entries pointing here
    pub direct: [u64; 12],    //direct block pointers
    pub indirect: u64,        //single indirect block pointer
    pub double_indirect: u64, //double indirect pointer
    pub _padding: [u8; 96],
}

//Directory Entry
pub struct DirEntry {
    pub inode: u32, //which inode entry this entry points to
    pub rec_len: u16,
    pub name_len: u8,    //length of the filename
    pub file_type: u8,   //file or directory
    pub name: [u8; 255], //filename (up to 255 chars)
}
