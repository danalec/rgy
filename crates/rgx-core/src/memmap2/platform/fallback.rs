//! Clean-room reimplementation of memmap2-rs fallback path.

use std::fs::File;
use std::io::{self, Read};
use std::ops::{Deref, DerefMut};

pub struct MmapInner {
    bytes: Vec<u8>,
}

unsafe impl Send for MmapInner {}
unsafe impl Sync for MmapInner {}

impl Drop for MmapInner {
    fn drop(&mut self) {}
}

impl MmapInner {
    pub fn map(_len: usize, _fd: &File, _offset: u64, _populate: bool) -> io::Result<MmapInner> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "fallback memmap requires the file to be opened by the caller",
        ))
    }

    pub fn map_exec(
        _len: usize,
        _fd: &File,
        _offset: u64,
        _populate: bool,
    ) -> io::Result<MmapInner> {
        Self::map(_len, _fd, _offset, _populate)
    }

    pub fn map_mut(
        _len: usize,
        _fd: &File,
        _offset: u64,
        _populate: bool,
    ) -> io::Result<MmapInner> {
        Self::map(_len, _fd, _offset, _populate)
    }

    pub fn map_copy(
        _len: usize,
        _fd: &File,
        _offset: u64,
        _populate: bool,
    ) -> io::Result<MmapInner> {
        Self::map(_len, _fd, _offset, _populate)
    }

    pub fn map_copy_read_only(
        _len: usize,
        _fd: &File,
        _offset: u64,
        _populate: bool,
    ) -> io::Result<MmapInner> {
        Self::map(_len, _fd, _offset, _populate)
    }

    pub fn map_anon(len: usize, _stack: bool, _populate: bool) -> io::Result<MmapInner> {
        if std::mem::size_of::<usize>() < 8 && len > std::isize::MAX as usize {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "memory map length overflows isize",
            ));
        }
        Ok(MmapInner {
            bytes: vec![0u8; len],
        })
    }

    pub fn map_file(file: &File, want: usize) -> io::Result<MmapInner> {
        let mut v: Vec<u8> = Vec::with_capacity(want);
        let mut f = file;
        f.read_to_end(&mut v)?;
        Ok(MmapInner { bytes: v })
    }

    pub fn flush(&self, _offset: usize, _len: usize) -> io::Result<()> {
        Ok(())
    }
    pub fn flush_async(&self, _offset: usize, _len: usize) -> io::Result<()> {
        Ok(())
    }
    pub fn make_read_only(&mut self) -> io::Result<()> {
        Ok(())
    }
    pub fn make_exec(&mut self) -> io::Result<()> {
        Ok(())
    }
    pub fn make_mut(&mut self) -> io::Result<()> {
        Ok(())
    }
    pub fn advise(&self, _advice: i32, _offset: usize, _len: usize) -> io::Result<()> {
        Ok(())
    }
    pub fn lock(&self) -> io::Result<()> {
        Ok(())
    }
    pub fn unlock(&self) -> io::Result<()> {
        Ok(())
    }

    #[inline]
    pub fn ptr(&self) -> *const u8 {
        self.bytes.as_ptr()
    }

    #[inline]
    pub fn mut_ptr(&mut self) -> *mut u8 {
        self.bytes.as_mut_ptr()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }
}

impl Deref for MmapInner {
    type Target = [u8];
    #[inline]
    fn deref(&self) -> &[u8] {
        &self.bytes
    }
}

impl DerefMut for MmapInner {
    #[inline]
    fn deref_mut(&mut self) -> &mut [u8] {
        &mut self.bytes
    }
}
