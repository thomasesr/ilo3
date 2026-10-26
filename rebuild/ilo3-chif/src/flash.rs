//! Host-side SPI reflash over CHIF (no resolder) — the firmware reflash tool.
//!
//! Erases + streams + verifies iLO's SPI payload from the host. The SPL/boot
//! region (>= SPL_BASE) is write-protected in firmware (dbgchif.rs), so a bad
//! payload reflash is itself re-flashable from here; only a nuked SPL needs the
//! SPI programmer. Mirror of the python tool's flash/verify.

use std::io::Write;
use std::time::Duration;

use crate::bar::Bar;
use crate::proto::*;

/// Clip a full-image write to [base, SPL_BASE) when it would cross into the
/// write-protected SPL/boot region.
fn clip_to_payload(base: u32, size: usize) -> usize {
    let end = base as u64 + size as u64;
    if base < SPL_BASE && end > SPL_BASE as u64 {
        let clipped = (SPL_BASE - base) as usize;
        eprintln!(
            "note: clipping to payload [{base:#x}, {SPL_BASE:#x}) \
             (SPL is write-protected; use the SPI programmer for it)"
        );
        clipped
    } else {
        size
    }
}

/// Reflash `data` at `base`: erase, stream, then verify.
pub fn flash(bar: &Bar, db: Option<&Doorbell>, path: &str, base: u32) -> bool {
    let mut data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("flash: cannot read {path}: {e}");
            return false;
        }
    };
    let size = clip_to_payload(base, data.len());
    data.truncate(size);
    println!("flash {path}: {size} B @ {base:#x}");

    let (st, _) = request(bar, db, CMD_FERASE, base, size as u32, &[], Duration::from_secs(60));
    if st != Some(0) {
        eprintln!("  erase FAILED status={st:?}");
        return false;
    }
    println!("  erased");

    let mut off = 0usize;
    while off < size {
        let end = (off + CHUNK).min(size);
        let chunk = &data[off..end];
        let (st, _) = request(
            bar, db, CMD_FWRITE, base + off as u32, chunk.len() as u32, chunk,
            Duration::from_secs(5),
        );
        if st != Some(0) {
            eprintln!("\n  write FAILED @ {:#x} status={st:?}", base + off as u32);
            return false;
        }
        print!("\r  wrote {end}/{size}");
        let _ = std::io::stdout().flush();
        off = end;
    }
    println!();
    verify(bar, db, &data, base)
}

/// Read back and compare `data` at `base`.
pub fn verify(bar: &Bar, db: Option<&Doorbell>, data: &[u8], base: u32) -> bool {
    let mut off = 0usize;
    while off < data.len() {
        let end = (off + CHUNK).min(data.len());
        let want = &data[off..end];
        let (st, got) = request(
            bar, db, CMD_FREAD, base + off as u32, want.len() as u32, &[],
            Duration::from_secs(5),
        );
        if st != Some(0) || got.len() < want.len() || &got[..want.len()] != want {
            eprintln!("\n  verify MISMATCH @ {:#x} status={st:?}", base + off as u32);
            return false;
        }
        print!("\r  verified {end}/{}", data.len());
        let _ = std::io::stdout().flush();
        off = end;
    }
    println!("\n  OK");
    true
}

/// Verify straight from a file on disk.
pub fn verify_file(bar: &Bar, db: Option<&Doorbell>, path: &str, base: u32) -> bool {
    let mut data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("verify: cannot read {path}: {e}");
            return false;
        }
    };
    data.truncate(clip_to_payload(base, data.len()));
    verify(bar, db, &data, base)
}
