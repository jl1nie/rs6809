//! A small binary format for saving the 68000 machines: fixed-width
//! big-endian integers and length-prefixed byte strings, in a known order.
//! Files start with a magic and a version so stale ones are refused.

use crate::tr;
pub const MAGIC: &[u8; 8] = b"RS68KST1";

#[derive(Default)]
pub struct Writer {
    pub out: Vec<u8>,
}

impl Writer {
    pub fn u8(&mut self, v: u8) {
        self.out.push(v)
    }
    pub fn bool(&mut self, v: bool) {
        self.out.push(u8::from(v))
    }
    pub fn u16(&mut self, v: u16) {
        self.out.extend(v.to_be_bytes())
    }
    pub fn u32(&mut self, v: u32) {
        self.out.extend(v.to_be_bytes())
    }
    pub fn u64(&mut self, v: u64) {
        self.out.extend(v.to_be_bytes())
    }
    pub fn bytes(&mut self, v: &[u8]) {
        self.u64(v.len() as u64);
        self.out.extend_from_slice(v)
    }
}

pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.data.len()).ok_or(tr!("状態のデータが途中で終わっている", "the state data ends early"))?;
        let s = &self.data[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    pub fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    pub fn bool(&mut self) -> Result<bool, String> {
        Ok(self.u8()? != 0)
    }
    pub fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub fn bytes(&mut self) -> Result<&'a [u8], String> {
        let n = self.u64()? as usize;
        self.take(n)
    }
    pub fn magic(&mut self, m: &[u8]) -> Result<(), String> {
        if self.take(m.len())? == m {
            Ok(())
        } else {
            Err(tr!("rs6809の68000の状態ファイルではない（または版が違う）", "not an rs6809 68000 state file (or another version)").into())
        }
    }
}
