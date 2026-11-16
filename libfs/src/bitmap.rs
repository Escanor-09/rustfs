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

    //alloc the first free bit among bits 0..limit
    //`limit` is how many real objects this bitmap tracks (e.g. total_inodes).
    //The block holds 32768 bits, but bits >= limit have no inode/data block
    //behind them, so handing one out would point past the end of the region.
    pub fn alloc(&mut self, limit: u64) -> Option<u64> {
        let limit = limit.min((self.data.len() * 8) as u64);
        let last_byte = ((limit + 7) / 8) as usize;

        for byte_index in 0..last_byte {
            if self.data[byte_index] != 0xff {
                //not all bits of the corresponding byte used
                for bit_index in 0..8 {
                    let n = (byte_index * 8 + bit_index) as u64;
                    if n >= limit {
                        return None;
                    }
                    //if free bit found
                    if (self.data[byte_index] >> bit_index) & 1 == 0 {
                        //assign that bit as 1
                        self.data[byte_index] |= 1 << bit_index;
                        return Some(n);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_stops_at_limit() {
        let mut bm = Bitmap::from_block([0u8; 4096]);
        // 2048 inodes: every one of them can be allocated...
        for expected in 0..2048 {
            assert_eq!(bm.alloc(2048), Some(expected));
        }
        // ...and the 2049th request reports "full" instead of handing out bit 2048
        assert_eq!(bm.alloc(2048), None);
        assert!(!bm.is_used(2048));
    }

    #[test]
    fn alloc_limit_not_multiple_of_8() {
        // 25461 data blocks is not a multiple of 8; the partial last byte must be bounded too
        let mut block = [0xffu8; 4096];
        block[25461 / 8] = 0x00; // bits 25456..25463 free, but only 25456..25460 are real
        let mut bm = Bitmap::from_block(block);
        for expected in 25456..25461 {
            assert_eq!(bm.alloc(25461), Some(expected));
        }
        assert_eq!(bm.alloc(25461), None);
    }

    #[test]
    fn free_bit_is_reused() {
        let mut bm = Bitmap::from_block([0u8; 4096]);
        bm.data[0] = 0b0000_0111; // like mkfs: inodes 0,1,2 reserved
        assert_eq!(bm.alloc(2048), Some(3));
        assert_eq!(bm.alloc(2048), Some(4));
        bm.free(3);
        assert_eq!(bm.alloc(2048), Some(3));
    }
}
