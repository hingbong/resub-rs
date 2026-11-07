//! Minimal EBML primitives: element ids, data sizes and fixed-width integers.

use std::io::{self, Read};

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

/// Read an element id, keeping the length-marker bits (ids are written literally,
/// e.g. 0x1A45DFA3 for the EBML header).
pub fn read_id<R: Read>(r: &mut R) -> io::Result<Option<u64>> {
    let mut first = [0u8; 1];
    if r.read(&mut first)? == 0 {
        return Ok(None);
    }
    let b = first[0];
    if b == 0 {
        return Err(bad("EBML id begins with 0x00"));
    }
    let width = b.leading_zeros() as usize + 1;
    if width > 8 {
        return Err(bad("EBML id too wide"));
    }
    let mut id = b as u64;
    for _ in 1..width {
        let mut x = [0u8; 1];
        r.read_exact(&mut x)?;
        id = (id << 8) | x[0] as u64;
    }
    Ok(Some(id))
}

/// Read a data size. None means "unknown size" (all value bits set), which streamed
/// Segments use; callers should then treat the element as extending to end of file.
pub fn read_size<R: Read>(r: &mut R) -> io::Result<Option<u64>> {
    let mut first = [0u8; 1];
    r.read_exact(&mut first)?;
    let b = first[0];
    if b == 0 {
        return Err(bad("EBML size begins with 0x00"));
    }
    let width = b.leading_zeros() as usize + 1;
    if width > 8 {
        return Err(bad("EBML size too wide"));
    }
    let mask: u8 = if width >= 8 { 0 } else { 0xFFu8 >> width };
    let mut value = (b & mask) as u64;
    let mut unknown = (b & mask) == mask;
    for _ in 1..width {
        let mut x = [0u8; 1];
        r.read_exact(&mut x)?;
        value = (value << 8) | x[0] as u64;
        if x[0] != 0xFF {
            unknown = false;
        }
    }
    if unknown {
        Ok(None)
    } else {
        Ok(Some(value))
    }
}

/// Big-endian unsigned integer of a known byte length.
pub fn read_uint<R: Read>(r: &mut R, len: u64) -> io::Result<u64> {
    let mut v: u64 = 0;
    for _ in 0..len {
        let mut x = [0u8; 1];
        r.read_exact(&mut x)?;
        v = (v << 8) | x[0] as u64;
    }
    Ok(v)
}

pub fn read_bytes<R: Read>(r: &mut R, len: u64) -> io::Result<Vec<u8>> {
    let len = usize::try_from(len).map_err(|_| bad("element too large"))?;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

/// Read-only cursor over an in-memory element payload, so cluster parsing needs no seeking.
pub struct SliceReader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> SliceReader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    pub fn pos(&self) -> usize {
        self.pos
    }
    pub fn remaining(&self) -> &'a [u8] {
        &self.buf[self.pos.min(self.buf.len())..]
    }
    pub fn is_empty(&self) -> bool {
        self.pos >= self.buf.len()
    }
    pub fn skip(&mut self, n: usize) {
        self.pos = (self.pos + n).min(self.buf.len());
    }
    /// Take n bytes and advance past them.
    pub fn take(&mut self, n: usize) -> &'a [u8] {
        let end = (self.pos + n).min(self.buf.len());
        let s = &self.buf[self.pos..end];
        self.pos = end;
        s
    }
}

impl<'a> Read for SliceReader<'a> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let n = out.len().min(self.buf.len().saturating_sub(self.pos));
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}
