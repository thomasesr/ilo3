//! iLO 3 (Gromit-XE) CHIF host tool — debug console + host-side SPI reflash over
//! PCI. Runs on the XCP-ng 8.3 (x86_64) host; talks to the clean-room Rust
//! firmware with no JTAG/UART. Compiled sibling of tools/ilo3-chif-console.py.

mod bar;
mod console;
mod flash;
mod proto;
mod recover;

use std::path::{Path, PathBuf};
use std::time::Duration;

use bar::Bar;
use proto::{Doorbell, REQ_CCB};

const USAGE: &str = "\
ilo3-chif — iLO 3 CHIF host tool (debug console + SPI reflash over PCI)

USAGE:
  ilo3-chif <mode> [args] [--bar <resource2>] [options]

MODES:
  observe                       passive hexdump of the CHIF window (no protocol)
  console                       interactive request/response shell
  recover --image <main.bin>    SPL RAM-boot a raw main image (no doorbell)
  ping | drain | id | reboot    one-shot debug commands
  md <addr> <len>               memory dump      (hex args)
  mw <addr> <u32> | mb <a> <u8> memory write
  flash <file> [base]           reflash iLO SPI payload (erase+write+verify)
  verify <file> [base]          read-back compare only

OPTIONS:
  --bar <path>      CHIF BAR2 resourceN file (default: auto-find 103c:3307 .2)
  --pci <bdf>       PCI address, e.g. 0000:02:00.2 (uses its resource2)
  --base <hex>      flash/verify base offset (default 0)
  --image <file>    raw main firmware for `recover`
  --db-bar <path>   doorbell BAR (default: resource1 beside --bar)
  --db-off <hex>    doorbell register offset  -> enables ringing (CONFIRM-ON-HW)
  --db-val <hex>    doorbell value (default 1<<REQ_CCB)

Run as root. Free the region first if hpilo holds it:
  echo <bdf> > /sys/bus/pci/drivers/hpilo/unbind";

struct Opts {
    mode: String,
    rest: Vec<String>,
    bar: Option<String>,
    pci: Option<String>,
    base: Option<u32>,
    image: Option<String>,
    db_bar: Option<String>,
    db_off: Option<usize>,
    db_val: Option<u32>,
}

fn hex(s: &str) -> u32 {
    u32::from_str_radix(s.trim_start_matches("0x"), 16).unwrap_or(0)
}

fn parse() -> Option<Opts> {
    let mut a = std::env::args().skip(1).peekable();
    let mode = a.next()?;
    if mode == "-h" || mode == "--help" {
        return None;
    }
    let mut o = Opts {
        mode, rest: vec![], bar: None, pci: None, base: None,
        image: None, db_bar: None, db_off: None, db_val: None,
    };
    while let Some(t) = a.next() {
        match t.as_str() {
            "--bar" => o.bar = a.next(),
            "--pci" => o.pci = a.next(),
            "--base" => o.base = a.next().map(|v| hex(&v)),
            "--image" => o.image = a.next(),
            "--db-bar" => o.db_bar = a.next(),
            "--db-off" => o.db_off = a.next().map(|v| hex(&v) as usize),
            "--db-val" => o.db_val = a.next().map(|v| hex(&v)),
            _ => o.rest.push(t),
        }
    }
    Some(o)
}

/// Find the CHIF .2 function (vendor 103c, device 3307) and return its resource2.
fn find_bar() -> Option<PathBuf> {
    let dir = Path::new("/sys/bus/pci/devices");
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        let v = std::fs::read_to_string(p.join("vendor")).unwrap_or_default();
        let d = std::fs::read_to_string(p.join("device")).unwrap_or_default();
        if v.trim() == "0x103c" && d.trim() == "0x3307" {
            return Some(p.join("resource2"));
        }
    }
    None
}

/// Derive the resourceN sibling of a resource path (for the doorbell BAR).
fn sibling(bar: &str, n: u8) -> String {
    match bar.rfind("resource") {
        Some(i) => format!("{}resource{n}", &bar[..i]),
        None => bar.to_string(),
    }
}

fn main() {
    let o = match parse() {
        Some(o) => o,
        None => {
            println!("{USAGE}");
            return;
        }
    };

    let bar_path = o
        .bar
        .clone()
        .or_else(|| o.pci.as_ref().map(|b| format!("/sys/bus/pci/devices/{b}/resource2")))
        .or_else(|| find_bar().map(|p| p.to_string_lossy().into_owned()));
    let bar_path = match bar_path {
        Some(p) => p,
        None => {
            eprintln!("no CHIF BAR: pass --bar <resource2> or --pci <bdf> (103c:3307 not found)");
            std::process::exit(2);
        }
    };
    let bar = match Bar::open(Path::new(&bar_path)) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("open {bar_path}: {e}\n(root? hpilo unbound?)");
            std::process::exit(2);
        }
    };

    // Optional doorbell (only when an offset is supplied — CONFIRM-ON-HW).
    let db_bar = o.db_off.map(|_| {
        let p = o.db_bar.clone().unwrap_or_else(|| sibling(&bar_path, 1));
        Bar::open(Path::new(&p)).unwrap_or_else(|e| {
            eprintln!("open doorbell bar {p}: {e}");
            std::process::exit(2);
        })
    });
    let db = o.db_off.map(|off| Doorbell {
        bar: db_bar.as_ref().unwrap(),
        off,
        val: o.db_val.unwrap_or(1 << REQ_CCB),
    });
    let db = db.as_ref();

    match o.mode.as_str() {
        "observe" => console::observe(&bar),
        "console" => console::console(&bar, db),
        "recover" => {
            let img = o.image.or_else(|| o.rest.first().cloned());
            match img {
                Some(i) => {
                    recover::recover(&bar, &i, Duration::from_secs(90));
                }
                None => eprintln!("recover needs --image <main.bin>"),
            }
        }
        _ => {
            // one-shot debug/flash command: [mode, positionals...]
            let mut tok: Vec<&str> = vec![o.mode.as_str()];
            tok.extend(o.rest.iter().map(|s| s.as_str()));
            let base_hex;
            if matches!(o.mode.as_str(), "flash" | "verify") && o.rest.len() == 1 {
                if let Some(b) = o.base {
                    base_hex = format!("{b:#x}");
                    tok.push(&base_hex);
                }
            }
            console::exec(&bar, db, &tok);
        }
    }
}
