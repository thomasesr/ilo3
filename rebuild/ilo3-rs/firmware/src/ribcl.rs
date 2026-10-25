//! RIBCL (XML scripting) handler (Phase 5, docs/37).
//!
//! HP's Remote Insight Board Command Language: an XML envelope
//! `<RIBCL><LOGIN><X_INFO MODE><COMMAND/></X_INFO></LOGIN></RIBCL>` → a
//! `<RESPONSE STATUS=.../>` plus data. Publicly documented (iLO 3 Scripting
//! Guide); built to that schema. Each request carries a LOGIN authenticated
//! against the account DB (webauth), then the command(s) run privilege-gated,
//! bound to the same HAL/services as the web/CLI/IPMI. A compact XML tokenizer
//! (no no_std XML crate needed for this flat subset) feeds the dispatcher.

#![allow(dead_code)]

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::eventlog::{self, LogKind};
use crate::users::Privileges;
use crate::{power, sensors, webauth};

struct Tag {
    name: String,
    attrs: Vec<(String, String)>,
    end: bool,   // </name>
}

/// Tokenize the XML into a flat tag stream (start/end/empty). Ignores text nodes,
/// `<?..?>` and `<!..>`. Empty tags (`<x/>`) appear as a start then an end.
fn tokenize(xml: &str) -> Vec<Tag> {
    let b = xml.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        let Some(close) = xml[i..].find('>') else { break };
        let inner = &xml[i + 1..i + close];
        i += close + 1;
        if inner.starts_with('?') || inner.starts_with('!') {
            continue;
        }
        let end = inner.starts_with('/');
        let empty = inner.ends_with('/');
        let body = inner.trim_start_matches('/').trim_end_matches('/').trim();
        let mut parts = body.splitn(2, char::is_whitespace);
        let name = parts.next().unwrap_or("").to_uppercase();
        let attrs = parts.next().map(parse_attrs).unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        out.push(Tag { name: name.clone(), attrs, end });
        if empty {
            out.push(Tag { name, attrs: Vec::new(), end: true });
        }
    }
    out
}

/// Parse `key="value"` pairs (keys upper-cased to match RIBCL).
fn parse_attrs(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(eq) = rest.find('=') {
        let key = rest[..eq].trim().to_uppercase();
        let after = rest[eq + 1..].trim_start();
        if let Some(q) = after.strip_prefix('"') {
            if let Some(end) = q.find('"') {
                out.push((key, q[..end].to_string()));
                rest = &q[end + 1..];
                continue;
            }
        }
        break;
    }
    out
}

fn attr<'a>(tag: &'a Tag, key: &str) -> Option<&'a str> {
    tag.attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn response(status: u16, msg: &str) -> String {
    format!("<RESPONSE STATUS=\"0x{:04X}\" MESSAGE=\"{}\"/>\n", status, msg)
}

/// Handle one RIBCL document, returning the response XML.
pub fn handle(xml: &str) -> String {
    let tags = tokenize(xml);

    // LOGIN credentials.
    let login = tags.iter().find(|t| t.name == "LOGIN" && !t.end);
    let (user, pass) = match login.and_then(|t| Some((attr(t, "USER_LOGIN")?, attr(t, "PASSWORD")?))) {
        Some(c) => c,
        None => return wrap(&response(0x0001, "missing or malformed LOGIN")),
    };
    let Some(privs) = webauth::authenticate(user, pass) else {
        return wrap(&response(0x0005, "login failed"));
    };

    // Execute each command tag (not the envelope/LOGIN/*_INFO wrappers).
    let mut body = String::new();
    let mut any = false;
    for t in tags.iter().filter(|t| !t.end) {
        if matches!(t.name.as_str(), "RIBCL" | "LOGIN") || t.name.ends_with("_INFO") {
            continue;
        }
        any = true;
        body.push_str(&exec(&t.name, privs));
    }
    if !any {
        body.push_str(&response(0x0000, "No error"));
    }
    wrap(&body)
}

fn wrap(inner: &str) -> String {
    format!("<?xml version=\"1.0\"?>\n<RIBCL VERSION=\"2.0\">\n{}</RIBCL>\n", inner)
}

/// Execute one RIBCL command, privilege-gated, bound to the HAL/services.
fn exec(cmd: &str, privs: Privileges) -> String {
    match cmd {
        "GET_FW_VERSION" => {
            "<GET_FW_VERSION FIRMWARE_VERSION=\"0.0.1\" MANAGEMENT_PROCESSOR=\"iLO3-rs\"/>\n".to_string()
        }
        "GET_EMBEDDED_HEALTH" => get_embedded_health(),
        "SET_HOST_POWER" => gated(privs, Privileges::RESET_SERVER, || {
            power::power_on(); // honors HOST_POWER attr in a fuller impl
            response(0x0000, "No error")
        }),
        "PRESS_PWR_BTN" => gated(privs, Privileges::RESET_SERVER, || {
            power::graceful_shutdown();
            response(0x0000, "No error")
        }),
        "COLD_BOOT_SERVER" => gated(privs, Privileges::RESET_SERVER, || {
            power::cold_cycle();
            response(0x0000, "No error")
        }),
        "CLEAR_IML" => gated(privs, Privileges::CLEAR_LOGS, || {
            eventlog::clear(LogKind::Iml);
            response(0x0000, "No error")
        }),
        "CLEAR_EVENTLOG" => gated(privs, Privileges::CLEAR_LOGS, || {
            eventlog::clear(LogKind::Ilo);
            response(0x0000, "No error")
        }),
        "RESET_RIB" => gated(privs, Privileges::CONFIG_ILO, || response(0x0000, "No error")),
        _ => response(0x0003, "command not implemented"),
    }
}

fn gated(have: Privileges, need: Privileges, f: impl FnOnce() -> String) -> String {
    if have.contains(need) {
        f()
    } else {
        response(0x0006, "insufficient privilege")
    }
}

fn get_embedded_health() -> String {
    let mut s = String::from("<GET_EMBEDDED_HEALTH_DATA>\n  <TEMPERATURE>\n");
    for sensor in sensors::TABLE {
        match sensor.read() {
            Ok(t) => s.push_str(&format!(
                "    <TEMP><LABEL VALUE=\"{}\"/><CURRENTREADING VALUE=\"{}\" UNIT=\"Celsius\"/></TEMP>\n",
                sensor.name, t
            )),
            Err(_) => s.push_str(&format!("    <TEMP><LABEL VALUE=\"{}\"/><STATUS VALUE=\"n/a\"/></TEMP>\n", sensor.name)),
        }
    }
    s.push_str("  </TEMPERATURE>\n</GET_EMBEDDED_HEALTH_DATA>\n");
    s.push_str(&response(0x0000, "No error"));
    s
}
