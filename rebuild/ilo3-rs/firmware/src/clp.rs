//! SMASH CLP command engine (Phase 5, docs/34).
//!
//! DMTF SM CLP: a CIM target tree (/map1 = the iLO MP, /system1 = the host)
//! navigated by verbs (cd/show/set/reset/...) plus HP extensions (power/uid/nmi).
//! Transport-agnostic: `execute(line) -> String`. The SSH server (russh, modern
//! KEX/ciphers only — no SSH-1/3DES) feeds lines + streams output; this engine is
//! the CLI logic, bound to the HAL and privilege-gated. Property names follow
//! standard SM CLP + HP extensions (RE just-in-time per target, docs/34).

#![allow(dead_code)]

use alloc::format;
use alloc::string::{String, ToString};

use crate::users::Privileges;
use crate::{cpld, power, sensors};

/// Valid navigable targets (for `cd`). A flat allowlist is enough for the subset.
const TARGETS: &[&str] = &[
    "/", "/map1", "/map1/accounts1", "/map1/nic1", "/map1/snmp1", "/map1/log1", "/map1/firmware1",
    "/system1", "/system1/cpu", "/system1/fan1", "/system1/led1", "/system1/drives1",
    "/lanendpt1",
];

/// One CLI session: current working target + the authenticated privileges.
pub struct ClpSession {
    cwd: String,
    privs: Privileges,
    pub closed: bool,
}

impl ClpSession {
    pub fn new(privs: Privileges) -> Self {
        ClpSession { cwd: "/".to_string(), privs, closed: false }
    }

    /// The prompt (current target).
    pub fn prompt(&self) -> String {
        format!("{}> ", self.cwd)
    }

    /// Execute one command line; returns the textual output.
    pub fn execute(&mut self, line: &str) -> String {
        let line = line.trim();
        if line.is_empty() {
            return String::new();
        }
        let mut parts = line.split_whitespace();
        let verb = parts.next().unwrap_or("");
        let args: alloc::vec::Vec<&str> = parts.collect();
        match verb {
            "help" => HELP.to_string(),
            "version" => "iLO3-rs SMASH CLP 1.0\n".to_string(),
            "exit" | "quit" => {
                self.closed = true;
                "goodbye\n".to_string()
            }
            "pwd" => format!("{}\n", self.cwd),
            "cd" => self.cd(args.first().copied().unwrap_or("/")),
            "show" => self.show(args.first().copied()),
            "set" => self.set(&args),
            "power" => self.power(args.first().copied()),
            "uid" => self.uid(args.first().copied()),
            "nmi" => self.nmi(),
            "vsp" | "textcons" => self.vsp(),
            _ => format!("error: unknown command '{}'\n", verb),
        }
    }

    fn require(&self, p: Privileges) -> Result<(), String> {
        if self.privs.contains(p) {
            Ok(())
        } else {
            Err("error: insufficient privilege\n".to_string())
        }
    }

    /// Resolve + normalize a path against the cwd; None if not a valid target.
    fn resolve(&self, path: &str) -> Option<String> {
        let mut segs: alloc::vec::Vec<&str> = alloc::vec::Vec::new();
        let base = if path.starts_with('/') { "/" } else { self.cwd.as_str() };
        for s in base.split('/').chain(path.split('/')) {
            match s {
                "" | "." => {}
                ".." => {
                    segs.pop();
                }
                other => segs.push(other),
            }
        }
        let norm = if segs.is_empty() { "/".to_string() } else { format!("/{}", segs.join("/")) };
        TARGETS.contains(&norm.as_str()).then_some(norm)
    }

    fn cd(&mut self, path: &str) -> String {
        match self.resolve(path) {
            Some(p) => {
                self.cwd = p;
                String::new()
            }
            None => "error: no such target\n".to_string(),
        }
    }

    fn show(&self, target: Option<&str>) -> String {
        if self.require(Privileges::LOGIN).is_err() {
            return "error: insufficient privilege\n".to_string();
        }
        let path = match target {
            Some(t) => match self.resolve(t) {
                Some(p) => p,
                None => return "error: no such target\n".to_string(),
            },
            None => self.cwd.clone(),
        };
        match path.as_str() {
            "/" => "targets:\n  map1\n  system1\n  lanendpt1\n".to_string(),
            "/map1" => "targets:\n  accounts1 nic1 snmp1 log1 firmware1\n".to_string(),
            "/system1" => {
                let state = power_state_str();
                format!("/system1\n  properties:\n    power_state={}\n  targets:\n    cpu fan1 led1 drives1\n", state)
            }
            "/system1/led1" => format!("/system1/led1\n  properties:\n    uid={}\n", if cpld::uid_led_on() { "on" } else { "off" }),
            "/system1/fan1" => {
                let mut s = String::from("/system1/fan1\n  properties:\n");
                for (i, sensor) in sensors::TABLE.iter().enumerate() {
                    let t = sensor.read().map(|v| v.to_string()).unwrap_or_else(|_| "n/a".to_string());
                    s.push_str(&format!("    sensor{}_{}={}\n", i, sensor.name, t));
                }
                s
            }
            _ => format!("{}\n  (no properties)\n", path),
        }
    }

    fn set(&self, args: &[&str]) -> String {
        if let Err(e) = self.require(Privileges::CONFIG_ILO) {
            return e;
        }
        // set <target> prop=value  (minimal: led1 uid=on/off)
        let prop = args.iter().find(|a| a.contains('='));
        match prop {
            Some(p) if p.starts_with("uid=") => {
                let on = p.ends_with("on");
                cpld::uid_led(on);
                format!("uid={}\n", if on { "on" } else { "off" })
            }
            _ => "error: unsupported property\n".to_string(),
        }
    }

    fn power(&self, action: Option<&str>) -> String {
        match action {
            None => format!("power: {}\n", power_state_str()),
            Some(a) => {
                if let Err(e) = self.require(Privileges::RESET_SERVER) {
                    return e;
                }
                match a {
                    "on" => power::power_on(),
                    "off" => power::force_off(),
                    "cycle" => power::cold_cycle(),
                    "reset" => power::warm_reset(),
                    "soft" => power::graceful_shutdown(),
                    _ => return "error: usage: power [on|off|cycle|reset|soft]\n".to_string(),
                }
                format!("power {} ok\n", a)
            }
        }
    }

    fn uid(&self, action: Option<&str>) -> String {
        match action {
            None => format!("uid: {}\n", if cpld::uid_led_on() { "on" } else { "off" }),
            Some(a) => {
                if let Err(e) = self.require(Privileges::CONFIG_ILO) {
                    return e;
                }
                match a {
                    "on" => cpld::uid_led(true),
                    "off" => cpld::uid_led(false),
                    _ => return "error: usage: uid [on|off]\n".to_string(),
                }
                format!("uid {}\n", a)
            }
        }
    }

    fn nmi(&self) -> String {
        if let Err(e) = self.require(Privileges::RESET_SERVER) {
            return e;
        }
        power::nmi();
        "nmi sent\n".to_string()
    }

    /// Enter virtual-serial / text-console passthrough (docs/34). The SSH/TCP
    /// layer switches to raw streaming after this acknowledgement; the bridge
    /// itself is `vsp.rs` (HW-gated, docs/45).
    fn vsp(&self) -> String {
        if let Err(e) = self.require(Privileges::REMOTE_CONSOLE) {
            return e;
        }
        "entering virtual serial console (VSP/SOL) — transport not yet wired\n".to_string()
    }
}

fn power_state_str() -> &'static str {
    match power::state() {
        power::State::On => "on",
        power::State::Off => "off",
        power::State::InReset => "in_reset",
    }
}

const HELP: &str = "verbs: cd show set reset power uid nmi version help exit\n\
targets: /map1 (iLO)  /system1 (host)  /lanendpt1\n\
vsp textcons: virtual-serial / text-console passthrough\n";
