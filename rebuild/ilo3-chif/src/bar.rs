//! mmap wrapper over a PCI BAR `resourceN` sysfs file (volatile MMIO access).
//!
//! PCI memory BARs are exposed by the kernel as `/sys/bus/pci/devices/<bdf>/
//! resourceN`; they support mmap but not read()/write(), so we map and poke.

use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr::{read_volatile, write_volatile};

pub struct Bar {
    base: *mut u8,
    len: usize,
    fd: libc::c_int,
}

impl Bar {
    /// Open and mmap a `resourceN` file read/write.
    pub fn open(path: &Path) -> io::Result<Bar> {
        let mut c = path.as_os_str().as_bytes().to_vec();
        c.push(0);
        // SAFETY: c is NUL-terminated; O_SYNC keeps MMIO writes uncached.
        let fd = unsafe { libc::open(c.as_ptr() as *const _, libc::O_RDWR | libc::O_SYNC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let len = bar_size(path, fd);
        // SAFETY: fd is a valid, writable file; len > 0.
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if p == libc::MAP_FAILED {
            let e = io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(e);
        }
        Ok(Bar { base: p as *mut u8, len, fd })
    }

    #[inline]
    fn at(&self, off: usize) -> *mut u8 {
        assert!(off + 4 <= self.len, "BAR access {off:#x} out of range");
        // SAFETY: bounds-checked above.
        unsafe { self.base.add(off) }
    }

    #[inline]
    pub fn r8(&self, off: usize) -> u8 {
        unsafe { read_volatile(self.at(off)) }
    }
    #[inline]
    pub fn r16(&self, off: usize) -> u16 {
        unsafe { read_volatile(self.at(off) as *const u16) }
    }
    #[inline]
    pub fn r32(&self, off: usize) -> u32 {
        unsafe { read_volatile(self.at(off) as *const u32) }
    }
    #[inline]
    pub fn w8(&self, off: usize, v: u8) {
        unsafe { write_volatile(self.at(off), v) }
    }
    #[inline]
    pub fn w16(&self, off: usize, v: u16) {
        unsafe { write_volatile(self.at(off) as *mut u16, v) }
    }
    #[inline]
    pub fn w32(&self, off: usize, v: u32) {
        unsafe { write_volatile(self.at(off) as *mut u32, v) }
    }

    /// Copy bytes into the BAR at `off`.
    pub fn write_bytes(&self, off: usize, data: &[u8]) {
        for (i, &b) in data.iter().enumerate() {
            self.w8(off + i, b);
        }
    }
    /// Read `len` bytes out of the BAR at `off`.
    pub fn read_bytes(&self, off: usize, len: usize) -> Vec<u8> {
        (0..len).map(|i| self.r8(off + i)).collect()
    }

    /// Sum of bytes [off, off+n) as u8 (checksum helper).
    pub fn sum8(&self, off: usize, n: usize) -> u8 {
        (0..n).fold(0u8, |a, i| a.wrapping_add(self.r8(off + i)))
    }
}

impl Drop for Bar {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.base as *mut _, self.len);
            libc::close(self.fd);
        }
    }
}

/// Determine the BAR size to mmap. `fstat` on a PCI `resourceN` file returns the
/// real size on current kernels, but some report 0 — then the 1 MB default would
/// fail `mmap` on a window that enumerated small (e.g. 4 KB), breaking the first-
/// 4KB recovery fallback. So prefer the authoritative `resource` table, fall back
/// to fstat, then a 1 MB default. (REVIEW.md blocker 1.)
fn bar_size(path: &Path, fd: libc::c_int) -> usize {
    if let Some(sz) = size_from_resource_table(path) {
        return sz;
    }
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut st) } == 0 && st.st_size > 0 {
        return st.st_size as usize;
    }
    0x10_0000 // 1 MB default (CHIF window)
}

/// Parse the BAR's size from the sibling `resource` table: one `start end flags`
/// line per BAR; size = end - start + 1. Returns None for a non-`resourceN` path
/// (e.g. a plain test file), so fstat handles those.
fn size_from_resource_table(path: &Path) -> Option<usize> {
    let n: usize = path.file_name()?.to_str()?.strip_prefix("resource")?.parse().ok()?;
    let text = std::fs::read_to_string(path.with_file_name("resource")).ok()?;
    let mut it = text.lines().nth(n)?.split_whitespace();
    let start = u64::from_str_radix(it.next()?.trim_start_matches("0x"), 16).ok()?;
    let end = u64::from_str_radix(it.next()?.trim_start_matches("0x"), 16).ok()?;
    (end > start).then_some((end - start + 1) as usize)
}
