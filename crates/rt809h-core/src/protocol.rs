//! Command protocol of the RT809H on-board microcontroller.
//!
//! Every command is a 10 byte frame `AA cmd p1 p2 chk t0 t1 t2 t3 t4` that the
//! MCU echoes back with the first byte replaced by `55`. The check byte is the
//! XOR of the nine other bytes, rotated left by three bits. If the check byte
//! is wrong the MCU answers with the expected value instead of `55` and does
//! not execute the command.
//!
//! Before each frame the host pulses ACBUS6 low (via MPSSE) to resynchronise
//! the MCU's receiver, then drops back to the FIFO mode stored in the FTDI
//! EEPROM.

use std::time::Duration;

use crate::ftdi::{BitMode, DeviceEntry, Ftdi};
use crate::{Error, Result};

pub const CMD_POWER_OFF: u8 = 0x01;
pub const CMD_AUTH: u8 = 0x15;
pub const CMD_CONTACT: u8 = 0x3A;
pub const CMD_SET_MODE: u8 = 0x52;
pub const CMD_SET_RAILS: u8 = 0x56;
pub const CMD_MEASURE: u8 = 0x58;
pub const CMD_LOAD_ALGO: u8 = 0x5A;
pub const CMD_RESET_ALGO: u8 = 0xA5;
pub const CMD_ALGO_INFO: u8 = 0xEA;
pub const CMD_DEVICE_INFO: u8 = 0xF5;

const PAD: [u8; 5] = [0xCD; 5];

/// Build a command frame with a valid check byte.
pub fn frame(cmd: u8, p1: u8, p2: u8, tail: [u8; 5]) -> [u8; 10] {
    let mut f = [
        0xAA, cmd, p1, p2, 0, tail[0], tail[1], tail[2], tail[3], tail[4],
    ];
    f[4] = check_byte(&f);
    f
}

pub fn check_byte(f: &[u8; 10]) -> u8 {
    f.iter()
        .enumerate()
        .filter(|(i, _)| *i != 4)
        .fold(0u8, |acc, (_, b)| acc ^ b)
        .rotate_left(3)
}

/// Supply voltage for the chip, in volts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Voltage {
    V1_8,
    V2_5,
    V3_3,
}

impl Voltage {
    pub const ALL: [Voltage; 3] = [Voltage::V1_8, Voltage::V2_5, Voltage::V3_3];

    pub fn volts(self) -> f32 {
        match self {
            Voltage::V1_8 => 1.8,
            Voltage::V2_5 => 2.5,
            Voltage::V3_3 => 3.3,
        }
    }

    /// DAC code used by the rail configuration (5 mV per step).
    fn rail_code(self) -> u16 {
        (self.volts() * 200.0).round() as u16
    }

    /// Input threshold code observed together with each supply level.
    fn level_code(self) -> u16 {
        match self {
            Voltage::V1_8 => 0xE0,
            Voltage::V2_5 => 0xD2,
            Voltage::V3_3 => 0xBE,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Voltage::V1_8 => "1.8 V",
            Voltage::V2_5 => "2.5 V",
            Voltage::V3_3 => "3.3 V",
        }
    }
}

impl std::fmt::Display for Voltage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Result of the socket contact test.
#[derive(Debug, Clone)]
pub struct ContactReport {
    /// Three per-pin scans of the 48 pin ZIF socket (bit set = no contact).
    pub scans: [u64; 3],
    /// Socket pins the programmer identified as the chip's ground pin.
    pub ground_pins: Vec<u8>,
    /// Second pin group reported by the scan (purpose not fully known).
    pub other_pins: Vec<u8>,
    pub raw: Vec<u8>,
}

impl ContactReport {
    fn parse(raw: Vec<u8>) -> Result<Self> {
        if raw.len() < 30 {
            return Err(Error::Protocol("short contact report".into()));
        }
        let mut scans = [0u64; 3];
        for (i, s) in scans.iter_mut().enumerate() {
            let b = &raw[i * 6..i * 6 + 6];
            *s = b.iter().fold(0u64, |acc, &x| (acc << 8) | x as u64);
        }
        let group = |o: usize| -> Vec<u8> {
            let n = (raw[o] as usize).min(5);
            raw[o + 1..o + 1 + n]
                .iter()
                .copied()
                .filter(|p| (1..=48).contains(p))
                .collect()
        };
        let ground_pins = group(18);
        let other_pins = group(24);
        Ok(Self {
            scans,
            ground_pins,
            other_pins,
            raw,
        })
    }

    /// True if ZIF pin `pin` (1..=48) showed a connection in a scan.
    fn seen(scan: u64, pin: u8) -> bool {
        scan & (1u64 << (pin - 1)) == 0
    }

    /// A pin is considered contacted when either of the first two scans
    /// sees it.
    pub fn pin_ok(&self, pin: u8) -> bool {
        Self::seen(self.scans[0], pin) || Self::seen(self.scans[1], pin)
    }

    /// ZIF pins that show any connection.
    pub fn connected_pins(&self) -> Vec<u8> {
        (1..=48).filter(|&p| self.pin_ok(p)).collect()
    }

    /// True if nothing at all is detected in the socket.
    pub fn socket_empty(&self) -> bool {
        self.connected_pins().is_empty()
    }

    /// Check the pins of a DIP/SOIC package with `pins` pins that is pushed
    /// to the bottom of the socket (the RT809H convention). Returns the ZIF
    /// numbers of pins without contact.
    pub fn check_bottom_aligned(&self, pins: u8) -> Vec<u8> {
        zif_pins_bottom_aligned(pins)
            .into_iter()
            .filter(|&p| !self.pin_ok(p))
            .collect()
    }

    /// True if a bottom-aligned package with `pins` pins looks rotated by
    /// 180 degrees (ground found where the supply pin should be).
    pub fn reversed(&self, pins: u8) -> bool {
        let gnd = package_to_zif(pins, pins / 2);
        let vcc = package_to_zif(pins, pins);
        self.ground_pins.contains(&vcc) && !self.ground_pins.contains(&gnd)
    }

    /// True if something is connected outside the area a bottom-aligned
    /// package with `pins` pins would occupy (chip placed too high).
    pub fn misplaced(&self, pins: u8) -> bool {
        let expected = zif_pins_bottom_aligned(pins);
        self.connected_pins().iter().any(|p| !expected.contains(p))
    }
}

/// ZIF socket pin numbers occupied by a bottom-aligned DIP package.
///
/// ZIF pins 1..24 run down the left column, 25..48 back up the right one.
pub fn zif_pins_bottom_aligned(pins: u8) -> Vec<u8> {
    let half = pins / 2;
    let left = (24 - half + 1)..=24;
    let right = 25..=(24 + half);
    left.chain(right).collect()
}

/// Map a package pin (1-based, DIP numbering) to its ZIF pin.
pub fn package_to_zif(pins: u8, pkg_pin: u8) -> u8 {
    let half = pins / 2;
    if pkg_pin <= half {
        24 - half + pkg_pin
    } else {
        24 + (pkg_pin - half)
    }
}

/// Low level connection to the programmer MCU.
pub struct Mcu {
    pub ftdi: Ftdi,
}

impl Mcu {
    pub fn open(entry: &DeviceEntry) -> Result<Self> {
        let mut ftdi = Ftdi::open(entry)?;
        ftdi.reset()?;
        ftdi.set_latency(2)?;
        ftdi.set_flow_rts_cts()?;
        ftdi.purge()?;
        Ok(Self { ftdi })
    }

    fn sync(&mut self) -> Result<()> {
        self.ftdi.set_bitmode(0xFF, BitMode::Mpsse)?;
        self.ftdi.write(&[0x82, 0x00, 0x40])?;
        self.ftdi.write(&[0x82, 0xFF, 0x00])?;
        self.ftdi.set_bitmode(0xFF, BitMode::Reset)?;
        std::thread::sleep(Duration::from_millis(20));
        Ok(())
    }

    /// Send a command frame and verify its echo.
    pub fn command(&mut self, cmd: u8, p1: u8, p2: u8, tail: [u8; 5]) -> Result<()> {
        self.sync()?;
        let f = frame(cmd, p1, p2, tail);
        self.ftdi.write(&f)?;
        let echo = self.ftdi.read_exact(10)?;
        if echo[0] != 0x55 || echo[1..] != f[1..] {
            return Err(Error::Protocol(format!(
                "command {cmd:02X} not acknowledged ({echo:02X?})"
            )));
        }
        log::trace!("cmd {f:02X?} ok");
        Ok(())
    }

    /// Fire-and-forget frame (used for commands without an echo).
    pub fn command_noack(&mut self, cmd: u8, p1: u8, p2: u8, tail: [u8; 5]) -> Result<()> {
        self.sync()?;
        self.ftdi.write(&frame(cmd, p1, p2, tail))
    }

    /// Name of the logic image currently loaded ("BASE" after power-up).
    pub fn algorithm(&mut self) -> Result<String> {
        self.command(CMD_ALGO_INFO, 0xCD, 0xCD, PAD)?;
        let d = self.ftdi.read_exact(256)?;
        let name: Vec<u8> = d[1..].iter().take_while(|&&b| b != 0).copied().collect();
        Ok(String::from_utf8_lossy(&name).into_owned())
    }

    /// Go back to the power-up logic image.
    pub fn reset_algorithm(&mut self) -> Result<()> {
        self.command(CMD_RESET_ALGO, 0xAA, 0x55, [0x00, 0xCD, 0xCD, 0xCD, 0xCD])?;
        self.ftdi.set_bitmode(0xFF, BitMode::Mpsse)?;
        self.ftdi.write(&[0x82, 0xFF, 0x40])?;
        std::thread::sleep(Duration::from_millis(60));
        self.ftdi.reset()?;
        Ok(())
    }

    /// Session handshake: the host sends 128 random bytes and gets one byte
    /// back. The answer is not needed by this driver.
    pub fn handshake(&mut self) -> Result<u8> {
        self.command(CMD_AUTH, 0xCD, 0xCD, PAD)?;
        let mut challenge = [0u8; 128];
        fill_random(&mut challenge);
        self.ftdi.write(&challenge)?;
        Ok(self.ftdi.read_exact(1)?[0])
    }

    /// 64 bytes of hardware information (serial number, board revision).
    pub fn device_info(&mut self) -> Result<Vec<u8>> {
        self.command(CMD_DEVICE_INFO, 0xCD, 0xCD, PAD)?;
        self.ftdi.read_exact(64)
    }

    /// Program the supply rails. `vcc` powers the chip, `vio` sets the logic
    /// level of the signal pins.
    pub fn set_rails(&mut self, vcc: Voltage, vio: Voltage) -> Result<Vec<u16>> {
        let mut words: [u16; 20] = [0x0211; 20];
        words[0] = 0x0010;
        words[1] = vio.level_code();
        words[2] = 0x00EA;
        words[15] = 0x0294;
        words[18] = vcc.rail_code();
        words[19] = vio.rail_code();
        let payload: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        self.command(CMD_SET_RAILS, 0x14, 0x00, PAD)?;
        self.ftdi.write(&payload)?;
        let back = self.ftdi.read_exact(40)?;
        Ok(back
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect())
    }

    /// Raw supply monitor readings (three 10-bit ADC values).
    pub fn measure(&mut self) -> Result<[u16; 3]> {
        self.command(CMD_MEASURE, 0x03, 0x00, PAD)?;
        let prev = self.ftdi.read_timeout;
        self.ftdi.read_timeout = Duration::from_secs(5);
        let d = self.ftdi.read_exact(6);
        self.ftdi.read_timeout = prev;
        let d = d?;
        let v = |i: usize| u16::from_le_bytes([d[i], d[i + 1]]) & 0x03FF;
        Ok([v(0), v(2), v(4)])
    }

    /// Scan the ZIF socket for pin contact.
    pub fn contact_test(&mut self) -> Result<ContactReport> {
        self.command(CMD_CONTACT, 0x00, 0x00, [0, 0, 0, 0, 0x14])?;
        self.ftdi.write(&[0u8; 12])?;
        ContactReport::parse(self.ftdi.read_exact(48)?)
    }

    /// Route the socket to the FTDI SPI lines (25-series serial flash).
    pub fn enter_spi(&mut self, size_code: u8) -> Result<()> {
        self.command(CMD_SET_MODE, 0x00, size_code, [1, 1, 1, 1, 1])
    }

    /// Route the socket for I2C (24-series EEPROM).
    pub fn enter_i2c(&mut self) -> Result<()> {
        self.command(CMD_SET_MODE, 0x00, 0x00, [1, 0, 0, 0, 0])
    }

    /// Switch the chip supply off and release the socket.
    pub fn power_off(&mut self, spi: bool) -> Result<()> {
        let t = if spi {
            [1, 1, 1, 1, 1]
        } else {
            [1, 0, 0, 0, 0]
        };
        self.command_noack(CMD_POWER_OFF, 0x00, 0x00, t)?;
        std::thread::sleep(Duration::from_millis(30));
        let _ = self.ftdi.read_available(Duration::from_millis(5));
        self.ftdi.set_bitmode(0xFF, BitMode::Mpsse)?;
        self.ftdi.write(&[0x82, 0x00, 0x80])?;
        std::thread::sleep(Duration::from_millis(30));
        self.ftdi.set_bitmode(0xFF, BitMode::Reset)?;
        Ok(())
    }
}

/// Non-cryptographic random bytes (the handshake only needs variety).
fn fill_random(buf: &mut [u8]) {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut seed = RandomState::new().build_hasher();
    seed.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    let mut x = seed.finish() | 1;
    for b in buf {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *b = (x >> 24) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_byte_matches_device() {
        let cases: [([u8; 10], u8); 5] = [
            (
                [0xaa, 0xea, 0xcd, 0xcd, 0, 0xcd, 0xcd, 0xcd, 0xcd, 0xcd],
                0x6c,
            ),
            (
                [0xaa, 0x56, 0x14, 0x00, 0, 0xcd, 0xcd, 0xcd, 0xcd, 0xcd],
                0x29,
            ),
            (
                [0xaa, 0x3a, 0x00, 0x00, 0, 0x00, 0x00, 0x00, 0x00, 0x14],
                0x24,
            ),
            (
                [0xaa, 0x52, 0x00, 0x00, 0, 0x01, 0x01, 0x01, 0x01, 0x01],
                0xcf,
            ),
            (
                [0xaa, 0xa5, 0xaa, 0x55, 0, 0x00, 0xcd, 0xcd, 0xcd, 0xcd],
                0x87,
            ),
        ];
        for (f, want) in cases {
            assert_eq!(check_byte(&f), want, "{f:02x?}");
        }
    }

    #[test]
    fn rail_codes() {
        assert_eq!(Voltage::V3_3.rail_code(), 0x294);
        assert_eq!(Voltage::V2_5.rail_code(), 0x1f4);
        assert_eq!(Voltage::V1_8.rail_code(), 0x168);
    }

    #[test]
    fn contact_mapping() {
        // Real scan of a SOIC-8 in the bottom of the socket with ZIF pin 25
        // not touching.
        // not touching; this chip was also inserted rotated by 180 degrees.
        let raw = hex("ff ff f1 1f ff ff ff ff f1 1f ff ff ff ff ff 7f ff ff 01 1c 00 00 00 00 02 18 19 00 00 00");
        let r = ContactReport::parse(raw).unwrap();
        assert_eq!(
            zif_pins_bottom_aligned(8),
            vec![21, 22, 23, 24, 25, 26, 27, 28]
        );
        assert_eq!(r.check_bottom_aligned(8), vec![21, 25]);
        assert!(!r.misplaced(8));
        assert!(r.reversed(8));
        // correctly inserted W25Q128
        let good = hex("ff ff f0 1f ff ff ff ff f0 0f ff ff ff ff f0 9f ff ff 01 18 00 00 00 00 01 15 00 00 00 00");
        let r = ContactReport::parse(good).unwrap();
        assert!(r.check_bottom_aligned(8).is_empty());
        assert!(!r.reversed(8));
        assert_eq!(package_to_zif(8, 1), 21);
        assert_eq!(package_to_zif(8, 8), 28);
    }

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace()
            .map(|x| u8::from_str_radix(x, 16).unwrap())
            .collect()
    }
}
