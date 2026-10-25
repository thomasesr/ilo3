//! IPMI 2.0 message core + command dispatch (Phase 5, docs/36).
//!
//! Transport-agnostic responder: a request (NetFn/LUN, Cmd, data) in, a response
//! (completion code + data) out. The KCS driver (kcs.rs), a future RMCP+ LAN
//! listener, and the CHIF host path all frame this same core. Standard IPMI 2.0 +
//! chassis/sensor commands are built to spec; HP OEM (OEMHP_*) is a later add
//! once the OEM NetFn/cmd bytes are RE'd (docs/36). Data paths reuse the HAL
//! (power docs/24, sensors docs/25).

#![allow(dead_code)]

use alloc::vec;
use alloc::vec::Vec;

use crate::{power, sensors};

// NetFn (even = request; response = request NetFn | 1).
pub const NETFN_CHASSIS: u8 = 0x00;
pub const NETFN_SENSOR: u8 = 0x04;
pub const NETFN_APP: u8 = 0x06;
pub const NETFN_STORAGE: u8 = 0x0A;

// Commands.
const CMD_GET_DEVICE_ID: u8 = 0x01;
const CMD_COLD_RESET: u8 = 0x02;
const CMD_GET_CHASSIS_STATUS: u8 = 0x01;
const CMD_CHASSIS_CONTROL: u8 = 0x02;
const CMD_GET_SENSOR_READING: u8 = 0x2D;

// Completion codes.
pub const CC_OK: u8 = 0x00;
pub const CC_INVALID_CMD: u8 = 0xC1;
pub const CC_OUT_OF_RANGE: u8 = 0xC9;
pub const CC_REQ_DATA_INVALID: u8 = 0xCC;
pub const CC_UNSPECIFIED: u8 = 0xFF;

/// A decoded IPMI request (KCS/LAN/CHIF all reduce to this).
pub struct Request<'a> {
    pub netfn: u8,
    pub lun: u8,
    pub cmd: u8,
    pub data: &'a [u8],
}

/// An IPMI response: completion code + response data.
pub struct Response {
    pub cc: u8,
    pub data: Vec<u8>,
}

impl Response {
    fn ok(data: Vec<u8>) -> Self {
        Response { cc: CC_OK, data }
    }
    fn err(cc: u8) -> Self {
        Response { cc, data: Vec::new() }
    }
}

/// Dispatch one request to its handler.
pub fn dispatch(req: &Request) -> Response {
    match (req.netfn, req.cmd) {
        (NETFN_APP, CMD_GET_DEVICE_ID) => get_device_id(),
        (NETFN_APP, CMD_COLD_RESET) => Response::ok(Vec::new()), // ack; no self-reset here
        (NETFN_CHASSIS, CMD_GET_CHASSIS_STATUS) => get_chassis_status(),
        (NETFN_CHASSIS, CMD_CHASSIS_CONTROL) => chassis_control(req.data),
        (NETFN_SENSOR, CMD_GET_SENSOR_READING) => get_sensor_reading(req.data),
        _ => Response::err(CC_INVALID_CMD),
    }
}

/// App / Get Device ID (RFC-ish fixed response).
fn get_device_id() -> Response {
    Response::ok(vec![
        0x20, // device id
        0x01, // device revision
        0x00, // fw major (bit7=0: normal operation)
        0x01, // fw minor (BCD)
        0x02, // IPMI version 2.0
        0x8F, // additional device support: chassis|FRU|SEL|SDR|sensor
        0x0B, 0x00, 0x00, // manufacturer IANA = HP (11), LSB first
        0x00, 0x00, // product id
    ])
}

/// Chassis / Get Chassis Status — current power state from the host-power HAL.
fn get_chassis_status() -> Response {
    let power_on = matches!(power::state(), power::State::On | power::State::InReset);
    // byte0: [0]=power on; byte1: last power event; byte2: misc chassis state.
    Response::ok(vec![if power_on { 0x01 } else { 0x00 }, 0x00, 0x00])
}

/// Chassis / Chassis Control — map the IPMI action to the host-power HAL.
/// NOTE: force-off / cold-cycle busy-wait seconds (docs/24) and briefly stall the
/// executor; acceptable for a rare control command.
fn chassis_control(data: &[u8]) -> Response {
    let Some(&action) = data.first() else {
        return Response::err(CC_REQ_DATA_INVALID);
    };
    match action {
        0x00 => power::force_off(),       // power down
        0x01 => power::power_on(),        // power up
        0x02 => power::cold_cycle(),      // power cycle
        0x03 => power::warm_reset(),      // hard reset
        0x05 => power::graceful_shutdown(), // soft / ACPI shutdown
        _ => return Response::err(CC_REQ_DATA_INVALID),
    }
    Response::ok(Vec::new())
}

/// Sensor / Get Sensor Reading — sensor number indexes sensors::TABLE.
fn get_sensor_reading(data: &[u8]) -> Response {
    let Some(&num) = data.first() else {
        return Response::err(CC_REQ_DATA_INVALID);
    };
    let Some(sensor) = sensors::TABLE.get(num as usize) else {
        return Response::err(CC_OUT_OF_RANGE);
    };
    match sensor.read() {
        // [reading, status(scanning+events enabled), threshold-status]
        Ok(t) => Response::ok(vec![t.clamp(0, 255) as u8, 0xC0, 0x00]),
        // reading/state unavailable (byte1 bit5)
        Err(_) => Response::ok(vec![0x00, 0x20, 0x00]),
    }
}
