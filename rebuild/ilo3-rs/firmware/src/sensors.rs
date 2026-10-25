//! Temperature sensors — read mechanisms + types (Phase 2, docs/25).
//!
//! Two sources (docs/25): CPU dies via **PECI** (crate::peci), board sensors via
//! **I2C LM75** (crate::i2c, behind PCA9544 muxes). The actual sensor topology —
//! which bus/addr/PECI-client, and the caution/critical thresholds — is **SDR /
//! board data** provisioned at runtime (docs/25, docs/42 "Open"), NOT in the
//! firmware image. This module provides the read mechanism + a descriptor type;
//! `TABLE` below is a flagged placeholder until a real SDR dump fills it in.

#![allow(dead_code)]

use crate::{i2c, peci};

/// Temperature in whole degrees Celsius (sufficient for threshold compares).
pub type Celsius = i16;

/// Sensor read failure — the thermal fail-safe treats this as "force fans max".
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct ReadError;

/// Caution / critical thresholds for one sensor (SDR data — placeholder values).
#[derive(Clone, Copy)]
pub struct Thresholds {
    pub caution: Celsius,
    pub critical: Celsius,
}

/// Where/how a sensor is read.
#[derive(Clone, Copy)]
pub enum Source {
    /// LM75-class I2C temp sensor on `engine` at 7-bit `addr`.
    Lm75 { engine: usize, addr: u8 },
    /// CPU die via PECI, client address (0x30 + socket).
    Peci { client: u8 },
}

/// A configured sensor: how to read it + its thresholds.
#[derive(Clone, Copy)]
pub struct Sensor {
    pub name: &'static str,
    pub source: Source,
    pub thr: Thresholds,
}

impl Sensor {
    /// Read the current temperature, or `ReadError` (→ thermal fail-safe = max).
    pub fn read(&self) -> Result<Celsius, ReadError> {
        match self.source {
            Source::Lm75 { engine, addr } => read_lm75(engine, addr),
            Source::Peci { client } => read_peci(client),
        }
    }
}

/// LM75: the temperature register is 2 bytes, MSB = signed integer °C (the low
/// byte's top bit is the 0.5° fraction, dropped here). Powers up pointed at the
/// temp register, so a bare 2-byte read returns it.
fn read_lm75(engine: usize, addr: u8) -> Result<Celsius, ReadError> {
    let mut buf = [0u8; 2];
    i2c::read(engine, addr, &mut buf).map_err(|_| ReadError)?;
    Ok(buf[0] as i8 as Celsius)
}

/// PECI GetTemp (cmd 0x01): the engine returns a value relative to Tjmax. The
/// exact response encoding/Tjmax base is board/CPU data (CONFIRM-ON-HW); here we
/// read the raw low byte as a placeholder magnitude.
fn read_peci(client: u8) -> Result<Celsius, ReadError> {
    let tx = [0x01u8]; // GetTemp
    let mut rx = [0u8; 2];
    peci::xfer(client, &tx, &mut rx).map_err(|_| ReadError)?;
    Ok(rx[0] as Celsius)
}

/// Board sensor table — PLACEHOLDER (docs/25/42 Open: real topology + thresholds
/// come from the board SDR). Replace with a live SDR dump before trusting on HW.
pub const TABLE: &[Sensor] = &[
    Sensor {
        name: "CPU0",
        source: Source::Peci { client: 0x30 },
        thr: Thresholds { caution: 90, critical: 100 },
    },
    Sensor {
        name: "CPU1",
        source: Source::Peci { client: 0x31 },
        thr: Thresholds { caution: 90, critical: 100 },
    },
    Sensor {
        name: "Ambient",
        source: Source::Lm75 { engine: 0, addr: 0x48 },
        thr: Thresholds { caution: 42, critical: 50 },
    },
];
