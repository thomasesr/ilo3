//! SPL recovery RAM-boot — stream a raw main image into DRAM and boot it, for a
//! main too broken to reach its own CHIF debug task. No doorbell dependency; a
//! seq-counter mailbox the SPL polls (spl/src/recovery.rs). Volatile: persist the
//! fix afterwards with `flash`. Reset iLO while this waits.

use std::io::Write;
use std::time::{Duration, Instant};

use crate::bar::Bar;

// Mailbox at the START of the window (first 4 KB) — matches spl/src/recovery.rs.
// Deliberately in the first 4 KB so it stays reachable even if the host BAR2 only
// enumerates at the default 4 KB (i.e. if the BAR size fix didn't take).
const REC: usize = 0x0000; // mailbox base in the shared window
const REC_MAGIC: usize = REC + 0x00;
const REC_REQ: usize = REC + 0x04;
const REC_CMD: usize = REC + 0x08;
const REC_ADDR: usize = REC + 0x0c;
const REC_LEN: usize = REC + 0x10;
const REC_RESP: usize = REC + 0x14;
const REC_STATUS: usize = REC + 0x18;
const REC_DATA: usize = REC + 0x40;

const RCVR: u32 = 0x5256_4352; // "RCVR" (LE)
const RC_WRITE: u32 = 1;
const RC_BOOT: u32 = 2;
const RC_PING: u32 = 3;
const RC_CHUNK: usize = 0xF00; // mailbox data area (recovery.rs MAX_CHUNK; fits 4 KB)
const RAM_LOAD: u32 = 0x2000_0000; // DRAM base: main's _start / load address

struct Mbox<'a> {
    bar: &'a Bar,
    seq: u32,
}

impl Mbox<'_> {
    /// One mailbox request. req_seq != resp_seq carries it to the SPL; the SPL
    /// echoes seq into resp when done. Returns Some(status) or None on timeout.
    fn req(&mut self, cmd: u32, addr: u32, data: &[u8], wait: bool, timeout: Duration) -> Option<u32> {
        self.seq = self.seq.wrapping_add(1);
        let seq = self.seq;
        if !data.is_empty() {
            self.bar.write_bytes(REC_DATA, data);
        }
        self.bar.w32(REC_CMD, cmd);
        self.bar.w32(REC_ADDR, addr);
        self.bar.w32(REC_LEN, data.len() as u32);
        self.bar.w32(REC_REQ, seq);
        if !wait {
            return Some(0);
        }
        let t0 = Instant::now();
        while t0.elapsed() < timeout {
            if self.bar.r32(REC_RESP) == seq {
                return Some(self.bar.r32(REC_STATUS));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        None
    }
}

pub fn recover(bar: &Bar, image_path: &str, connect_timeout: Duration) -> bool {
    let data = match std::fs::read(image_path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("recover: cannot read {image_path}: {e}");
            return false;
        }
    };
    println!("recover: {} B -> DRAM {RAM_LOAD:#x} (volatile RAM-boot)", data.len());
    bar.w32(REC_RESP, 0);
    bar.w32(REC_REQ, 0);
    let mut mb = Mbox { bar, seq: 0 };
    bar.w32(REC_MAGIC, RCVR);

    println!("waiting for the SPL recovery window — reset/power-cycle iLO now...");
    let t0 = Instant::now();
    let mut connected = false;
    while t0.elapsed() < connect_timeout {
        bar.w32(REC_MAGIC, RCVR); // keep raised across window re-mapping
        if mb.req(RC_PING, 0, &[], true, Duration::from_millis(500)) == Some(0) {
            connected = true;
            break;
        }
    }
    if !connected {
        eprintln!("no SPL window (PCI enumeration / window timing — see docs/48)");
        return false;
    }
    println!("connected to SPL recovery");

    let mut off = 0usize;
    while off < data.len() {
        let end = (off + RC_CHUNK).min(data.len());
        let chunk = &data[off..end];
        let st = mb.req(RC_WRITE, RAM_LOAD + off as u32, chunk, true, Duration::from_secs(3));
        if st != Some(0) {
            eprintln!("\n  write FAILED @ {:#x} status={st:?}", RAM_LOAD + off as u32);
            return false;
        }
        print!("\r  streamed {end}/{}", data.len());
        let _ = std::io::stdout().flush();
        off = end;
    }
    println!();
    mb.req(RC_BOOT, RAM_LOAD, &[], false, Duration::from_millis(0));
    println!(
        "boot sent -> iLO RAM-booting {RAM_LOAD:#x} \
         (volatile; persist with `flash` once it's healthy)"
    );
    true
}
