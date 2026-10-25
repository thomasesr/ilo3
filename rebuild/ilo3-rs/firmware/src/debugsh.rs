//! Serial debug shell — the bring-up console (board UART, no host/network deps).
//!
//! An interactive REPL on the Gromit-XE console UART (0xC00000F0). It depends on
//! NOTHING that is HW-gated (no DDR ring, no CHIF, no TLS) — just the polled UART
//! — so it is the one channel that works from first boot, over a $3 USB-serial
//! tap on the board (docs/48). It is how you poke registers, reconstruct the MAC
//! ring, read the event log, etc. during bring-up.
//!
//! DANGER: `md`/`mw`/`mb` are raw peek/poke over ALL address space (like U-Boot).
//! That is the point for bring-up, but it is also arbitrary-memory access over the
//! serial line. This module is gated behind the `debug-shell` cargo feature so
//! production builds exclude it; never ship it enabled on a networked path.

#![allow(dead_code)]

use core::fmt::Write;
use core::ptr::{read_volatile, write_volatile};

use alloc::string::String;
use alloc::vec::Vec;
use embassy_time::{Duration, Timer};

use crate::uart::Uart;
use crate::{eventlog, power, sensors, spinor};

const PROMPT: &str = "ilo3> ";
const MAX_LINE: usize = 128;

fn parse_u32(s: &str) -> Option<u32> {
    let s = s.trim();
    let s = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s);
    u32::from_str_radix(s, 16).ok()
}

/// Dump `len` bytes from `addr`, 16/line, hex + ascii. Raw MMIO reads.
fn cmd_md(con: &Uart, addr: u32, len: u32) {
    let mut a = addr;
    let end = addr.wrapping_add(len.max(1));
    while a < end {
        let mut line = String::new();
        let _ = write!(line, "{:08x}: ", a);
        let mut ascii = String::new();
        for i in 0..16 {
            if a + i >= end {
                let _ = write!(line, "   ");
                continue;
            }
            // SAFETY: raw debug read; an unmapped addr can fault (bring-up tool).
            let b = unsafe { read_volatile((a + i) as *const u8) };
            let _ = write!(line, "{:02x} ", b);
            ascii.push(if (0x20..0x7f).contains(&b) { b as char } else { '.' });
        }
        let _ = writeln!(con_w(con), "{} {}", line, ascii);
        a = a.wrapping_add(16);
    }
}

// Uart is a Copy-ish zero-size handle; grab a writable one for writeln!.
fn con_w(_c: &Uart) -> Uart {
    Uart::console()
}

fn execute(con: &Uart, line: &str) {
    let mut w = Uart::console();
    let mut it = line.split_whitespace();
    let Some(cmd) = it.next() else { return };
    let args: Vec<&str> = it.collect();
    match cmd {
        "help" | "?" => {
            let _ = writeln!(w, "md <addr> [len]  mw <addr> <u32>  mb <addr> <u8>");
            let _ = writeln!(w, "id  temp  power  log  help");
        }
        "md" => match args.first().and_then(|a| parse_u32(a)) {
            Some(addr) => {
                let len = args.get(1).and_then(|a| parse_u32(a)).unwrap_or(64);
                cmd_md(con, addr, len);
            }
            None => {
                let _ = writeln!(w, "usage: md <addr> [len]");
            }
        },
        "mw" | "mb" => match (args.first().and_then(|a| parse_u32(a)), args.get(1).and_then(|a| parse_u32(a))) {
            (Some(addr), Some(val)) => {
                // SAFETY: raw debug write (bring-up tool, physical console only).
                unsafe {
                    if cmd == "mw" {
                        write_volatile(addr as *mut u32, val);
                    } else {
                        write_volatile(addr as *mut u8, val as u8);
                    }
                }
                let _ = writeln!(w, "[{:08x}] <- {:#x}", addr, val);
            }
            _ => {
                let _ = writeln!(w, "usage: {} <addr> <val>", cmd);
            }
        },
        "id" => match spinor::read_id() {
            Ok(id) => {
                let _ = writeln!(w, "flash JEDEC: {:02x} {:02x} {:02x}", id[0], id[1], id[2]);
            }
            Err(_) => {
                let _ = writeln!(w, "flash read_id failed");
            }
        },
        "temp" => {
            for s in sensors::TABLE {
                match s.read() {
                    Ok(t) => {
                        let _ = writeln!(w, "{}: {} C", s.name, t);
                    }
                    Err(_) => {
                        let _ = writeln!(w, "{}: READ FAIL", s.name);
                    }
                }
            }
        }
        "power" => {
            let s = match power::state() {
                power::State::On => "on",
                power::State::Off => "off",
                power::State::InReset => "in_reset",
            };
            let _ = writeln!(w, "host power: {}", s);
        }
        "log" => {
            for r in eventlog::snapshot(eventlog::LogKind::Iml) {
                let _ = writeln!(w, "#{} c{:#x}/{:#x} x{} {}", r.number, r.class, r.code, r.count, r.desc);
            }
        }
        _ => {
            let _ = writeln!(w, "?{}  (try: help)", cmd);
        }
    }
}

/// The debug-shell task: line-edit on the console UART, dispatch commands.
#[embassy_executor::task]
pub async fn debug_task() {
    let con = Uart::console();
    let mut w = Uart::console();
    let _ = writeln!(w, "\n[dbg] serial debug shell ready — 'help'");
    let _ = w.write_bytes(PROMPT.as_bytes());
    let mut line = String::with_capacity(MAX_LINE);
    loop {
        match con.read_byte() {
            Some(b'\r') | Some(b'\n') => {
                let _ = writeln!(w);
                if !line.is_empty() {
                    execute(&con, &line);
                    line.clear();
                }
                let _ = w.write_bytes(PROMPT.as_bytes());
            }
            Some(0x08) | Some(0x7f) => {
                // backspace / DEL
                if line.pop().is_some() {
                    let _ = w.write_bytes(b"\x08 \x08");
                }
            }
            Some(c) if (0x20..0x7f).contains(&c) && line.len() < MAX_LINE => {
                line.push(c as char);
                w.write_byte(c); // echo
            }
            Some(_) => {} // ignore other control bytes
            None => Timer::after(Duration::from_millis(10)).await, // idle poll
        }
    }
}
