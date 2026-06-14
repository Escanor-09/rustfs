use std::fs::OpenOptions;
use std::io::{self, Read, Seek, SeekFrom, Write};

pub struct Disk {
    file: std::fs::File,
}

impl Disk {
    pub fn create(path: &str, size: u64) -> io::Result<Disk> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        file.set_len(size)?;
        Ok(Disk { file })
    }
    //open an existing file
    pub fn open(path: &str) -> io::Result<Disk> {
        let file1 = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?;
        Ok(Disk { file: file1 })
    }

    //read one block from disk
    pub fn read_block(&mut self, block_num: u64) -> io::Result<[u8; 4096]> {
        let offset = block_num * 4096;
        self.file.seek(SeekFrom::Start(offset))?;
        let mut buf = [0u8; 4096];
        self.file.read_exact(&mut buf)?;
        Ok(buf)
    }

    //write one block to disk
    pub fn write_block(&mut self, block_num: u64, data: &[u8; 4096]) -> io::Result<()> {
        let offset = block_num * 4096;
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.write_all(data)?;
        Ok(())
    }
}
