//! CHIF debug-channel wire protocol — host side.
//!
//! Exact mirror of firmware/src/chif.rs (framing + CCB ownership) and
//! src/dbgchif.rs (command/status set). Keep the two in lock-step.
//!
//! Per-message layout in the shared window (little-endian):
//!   +0x00 u16 payload length   +0x06 u16 channel id   +0x08.. payload
//! Request payload : [cmd u8][addr u32][arg u32][data..]
//! Response payload: [status u8][len u16][data..]

use std::time::{Duration, Instant};

use crate::bar::Bar;

pub const DATA_REGION: usize = 0x4000; // global control header @ shmem+0x4000
pub const MAGIC: [u8; 4] = *b"CHIF"; // 0x18 message type
pub const NR_CCB: usize = 32;
pub const CCB_SIZE: usize = 0x80;
pub const CCB_OFFSET: usize = 0x04; // u32 message offset into the window
pub const CCB_OWN: usize = 0x10; // u32 ring control, bit31 = ownership
pub const OWN_BIT: u32 = 1 << 31;
pub const DBG_CH: u16 = 0xDB90; // debug channel id (dbgchif.rs)

pub const REQ_CCB: usize = 1; // host writes requests on this CCB
pub const REQ_MSG_OFF: usize = 0x8000 + REQ_CCB * 0x4000; // its message region

// Command opcodes (dbgchif.rs).
pub const CMD_PING: u8 = 0x01;
pub const CMD_DRAIN: u8 = 0x02;
pub const CMD_MD: u8 = 0x03;
pub const CMD_MW: u8 = 0x04;
pub const CMD_MB: u8 = 0x05;
pub const CMD_FID: u8 = 0x10;
pub const CMD_FERASE: u8 = 0x11;
pub const CMD_FWRITE: u8 = 0x12;
pub const CMD_FREAD: u8 = 0x13;
pub const CMD_REBOOT: u8 = 0x14;

/// The write-protected SPL/boot region — the firmware refuses host writes here
/// (dbgchif.rs hits_spl); a full-image flash is clipped to below this.
pub const SPL_BASE: u32 = 0x7f_0000;
pub const CHUNK: usize = 4096; // per fwrite/fread (well under the 16 KB region)

/// Host→BMC doorbell. The BMC reads `0x01F03004` bit2 (chif.rs rx_pending), set
/// from the host via a register in a small `.2` BAR. The exact BAR/offset/value
/// is CONFIRM-ON-HW (oracle: drivers/misc/hpilo.c `hpilo_db_outbound`); supply
/// it with --db-bar/--db-off/--db-val once pinned. Absent, `observe` still works
/// and a firmware that polls CCB ownership directly still answers.
pub struct Doorbell<'a> {
    pub bar: &'a Bar,
    pub off: usize,
    pub val: u32,
}

impl Doorbell<'_> {
    fn ring(&self) {
        self.bar.w32(self.off, self.val);
    }
}

/// Write the global control header: magic + a 14-byte sum-to-zero checksum.
fn write_header(bar: &Bar) {
    bar.write_bytes(DATA_REGION, &MAGIC);
    let chk = 0u8.wrapping_sub(bar.sum8(DATA_REGION, 13));
    bar.w8(DATA_REGION + 13, chk);
}

/// Send one request, wait for the response on the same CCB.
/// Returns (Some(status), data) on success, (None, []) on timeout.
pub fn request(
    bar: &Bar,
    db: Option<&Doorbell>,
    cmd: u8,
    addr: u32,
    arg: u32,
    data: &[u8],
    timeout: Duration,
) -> (Option<u8>, Vec<u8>) {
    let msg = REQ_MSG_OFF;
    let plen = (9 + data.len()) as u16;
    bar.w16(msg, plen);
    bar.w16(msg + 0x06, DBG_CH);
    bar.w8(msg + 0x08, cmd);
    bar.w32(msg + 0x09, addr);
    bar.w32(msg + 0x0d, arg);
    if !data.is_empty() {
        bar.write_bytes(msg + 0x11, data);
    }
    write_header(bar);
    // Hand the CCB to the BMC: set the ownership bit (bit31 = BMC owns).
    bar.w32(REQ_CCB * CCB_SIZE + CCB_OFFSET, msg as u32);
    bar.w32(REQ_CCB * CCB_SIZE + CCB_OWN, OWN_BIT);
    if let Some(d) = db {
        d.ring();
    }
    // Ownership convention (matches firmware net behaviour): the BMC consumes the
    // request (dbgchif_task: poll_rx sees OWN set), writes the response into the
    // same message region, then rx_release() CLEARS the bit — so OWN going back to
    // 0 means "host owns again = response ready". Wait for the clear.
    let t0 = Instant::now();
    while t0.elapsed() < timeout {
        if bar.r32(REQ_CCB * CCB_SIZE + CCB_OWN) & OWN_BIT == 0 {
            let roff = bar.r32(REQ_CCB * CCB_SIZE + CCB_OFFSET) as usize;
            let status = bar.r8(roff + 0x08);
            let n = bar.r16(roff + 0x09) as usize;
            let out = bar.read_bytes(roff + 0x0b, n);
            return (Some(status), out);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    (None, Vec::new())
}
