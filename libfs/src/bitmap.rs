pub struct Bitmap {
    data: [u8; 4096], //one block = 4096 bytes = 32768bits = 32768 inodes/block
}

impl Bitmap {
    //read the bytes from the disk block
    pub fn from_block(block: [u8; 4096]) -> Bitmap {
        Bitmap { data: block }
    }

    //write bitmap back to the disk
    pub fn to_block(&self) -> [u8; 4096] {
        self.data
    }

    //alloc the first free bit found in the bitmap
    pub fn alloc(&mut self) -> Option<u64> {
        for byte_index in 0..4096 {
            if self.data[byte_index] != 0xff {
                //not all bits of the corresponding byte used
                for bit_index in 0..8 {
                    //if free bit found
                    if (self.data[byte_index] >> bit_index) & 1 == 0 {
                        //assign that bit as 1
                        self.data[byte_index] |= 1 << bit_index;
                        return Some((byte_index * 8 + bit_index) as u64);
                    }
                }
            }
        }
        None
    }

    pub fn free(&mut self, n: u64) {
        let byte_index = (n / 8) as usize;
        let bit_index = (n % 8) as usize;
        self.data[byte_index] &= !(1u8 << bit_index);
    }

    pub fn is_used(&self, n: u64) -> bool {
        let byte_index = (n / 8) as usize;
        let bit_index = (n % 8) as usize;
        (self.data[byte_index] >> bit_index) & 1 == 1
    }
}
