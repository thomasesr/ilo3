//! Passive `observe` + interactive `console` modes.

use std::io::{BufRead, Write};
use std::time::Duration;

use crate::bar::Bar;
use crate::flash;
use crate::proto::*;

/// Hexdump `len` bytes of the mapped window at `base`.
pub fn observe(bar: &Bar) {
    println!("== control header @ {DATA_REGION:#x} ==");
    for off in (0..32).step_by(16) {
        let row = bar.read_bytes(DATA_REGION + off, 16);
        print_row(DATA_REGION + off, &row);
    }
    println!("== CCBs (offset, ownership) ==");
    for i in 0..NR_CCB {
        let b = i * CCB_SIZE;
        let off = bar.r32(b + CCB_OFFSET);
        let own = bar.r32(b + CCB_OWN);
        if off != 0 || own != 0 {
            let flag = if own & OWN_BIT != 0 { "  (produced)" } else { "" };
            println!("  ccb[{i:2}] off={off:#08x} own={own:#010x}{flag}");
        }
    }
}

fn print_row(base: usize, row: &[u8]) {
    let hexs: Vec<String> = row.iter().map(|b| format!("{b:02x}")).collect();
    let asc: String = row
        .iter()
        .map(|&b| if (32..127).contains(&b) { b as char } else { '.' })
        .collect();
    println!("{base:06x}: {:<48} {asc}", hexs.join(" "));
}

fn hexbytes(data: &[u8], base: u32) {
    for off in (0..data.len()).step_by(16) {
        let end = (off + 16).min(data.len());
        let hexs: Vec<String> = data[off..end].iter().map(|b| format!("{b:02x}")).collect();
        println!("{:08x}: {}", base + off as u32, hexs.join(" "));
    }
}

fn parse_hex(s: Option<&&str>) -> u32 {
    s.and_then(|v| u32::from_str_radix(v.trim_start_matches("0x"), 16).ok()).unwrap_or(0)
}

pub fn console(bar: &Bar, db: Option<&Doorbell>) {
    println!("iLO3 CHIF debug console. commands:");
    println!("  ping | drain | id | md <a> <n> | mw <a> <v> | mb <a> <v>");
    println!("  flash <file> [base_hex] | verify <file> [base_hex] | reboot | quit");
    if db.is_none() {
        eprintln!("note: doorbell-free mode — the firmware debug channel polls CCB");
        eprintln!("      ownership (dbgchif poll_rx_polled), so no BAR3 doorbell is needed.");
    }
    let stdin = std::io::stdin();
    loop {
        print!("chif> ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if stdin.lock().read_line(&mut line).unwrap_or(0) == 0 {
            break; // EOF
        }
        let tok: Vec<&str> = line.split_whitespace().collect();
        if tok.is_empty() {
            continue;
        }
        if !exec(bar, db, &tok) {
            break;
        }
    }
}

/// Run one command (shared by the interactive loop and one-shot CLI mode).
/// Returns false to quit.
pub fn exec(bar: &Bar, db: Option<&Doorbell>, tok: &[&str]) -> bool {
    match tok[0] {
        "quit" | "exit" => return false,
        "flash" if tok.len() > 1 => {
            flash::flash(bar, db, tok[1], parse_hex(tok.get(2)));
        }
        "verify" if tok.len() > 1 => {
            flash::verify_file(bar, db, tok[1], parse_hex(tok.get(2)));
        }
        "reboot" => {
            request(bar, db, CMD_REBOOT, 0, 0, &[], Duration::from_secs(1));
            println!("reboot sent (iLO re-runs flash boot; reconnect after boot)");
        }
        other => run_simple(bar, db, other, tok),
    }
    true
}

/// One request/response command (ping/drain/id/md/mw/mb).
fn run_simple(bar: &Bar, db: Option<&Doorbell>, cmd: &str, tok: &[&str]) {
    let code = match cmd {
        "ping" => CMD_PING,
        "drain" => CMD_DRAIN,
        "id" => CMD_FID,
        "md" => CMD_MD,
        "mw" => CMD_MW,
        "mb" => CMD_MB,
        _ => {
            println!("?");
            return;
        }
    };
    let addr = parse_hex(tok.get(1));
    let arg = parse_hex(tok.get(2));
    let (st, data) = request(bar, db, code, addr, arg, &[], Duration::from_secs(5));
    match st {
        None => println!("timeout (doorbell/convention not confirmed? try `observe`)"),
        Some(_) if cmd == "drain" => print!("{}", String::from_utf8_lossy(&data)),
        Some(_) if cmd == "md" => hexbytes(&data, addr),
        Some(_) if cmd == "id" => {
            let h: Vec<String> = data.iter().map(|b| format!("{b:02x}")).collect();
            println!("JEDEC id: {}", h.join(" "));
        }
        Some(s) if cmd == "ping" => {
            println!("status={s} \"{}\"", String::from_utf8_lossy(&data));
        }
        Some(s) => println!("status={s} data={data:?}"),
    }
}
