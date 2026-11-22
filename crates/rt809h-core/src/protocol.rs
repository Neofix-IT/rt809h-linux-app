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

/// Mode byte of `CMD_SET_MODE` that hands the socket to the MPSSE SPI
/// engine. Without bit 7 the SPI lines stay disconnected (reads return 0).
const SPI_MODE: u8 = 0xC0;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
            Voltage::V3_3 => 0xC2,
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
///
/// Bytes 8..16 of the reply hold a little-endian contact map of the bottom
/// eight socket rows: bit `k` (0..8) is ZIF pin `25 + k` (right column,
/// counted from the bottom up) and bit `8 + k` is ZIF pin `17 + k` (left
/// column, top down). A cleared bit means a chip pin was detected.
#[derive(Debug, Clone)]
pub struct ContactReport {
    pub map: u64,
    pub raw: Vec<u8>,
}

/// Where an 8-pin package was found in the socket.
#[derive(Debug, Clone, PartialEq)]
pub struct Placement8 {
    /// ZIF pin under chip pin 1 (21 = bottom of the socket).
    pub row: u8,
    /// Chip pins (1..=8) without contact.
    pub missing: Vec<u8>,
}

impl ContactReport {
    pub fn parse(raw: Vec<u8>) -> Result<Self> {
        if raw.len() < 16 {
            return Err(Error::Protocol("short contact report".into()));
        }
        let map = u64::from_le_bytes(raw[8..16].try_into().unwrap());
        Ok(Self { map, raw })
    }

    /// ZIF pins (17..=32) where a chip pin was detected.
    pub fn connected_pins(&self) -> Vec<u8> {
        let mut pins: Vec<u8> = (0..8)
            .filter(|k| self.map & (1 << (8 + k)) == 0)
            .map(|k| 17 + k as u8)
            .chain(
                (0..8)
                    .filter(|k| self.map & (1 << k) == 0)
                    .map(|k| 25 + k as u8),
            )
            .collect();
        pins.sort_unstable();
        pins
    }

    pub fn socket_empty(&self) -> bool {
        self.connected_pins().is_empty()
    }

    /// Best matching position of an 8-pin package, or `None` if nothing
    /// resembling one is in the socket.
    pub fn locate_8pin(&self) -> Option<Placement8> {
        let pins = self.connected_pins();
        let mut best: Option<(usize, Placement8)> = None;
        for row in 17..=21u8 {
            let missing: Vec<u8> = (1..=8)
                .filter(|&p| !pins.contains(&zif_pin_8(row, p)))
                .collect();
            let hits = 8 - missing.len();
            if best.as_ref().is_none_or(|(h, _)| hits > *h) {
                best = Some((hits, Placement8 { row, missing }));
            }
        }
        best.filter(|(h, _)| *h >= 5).map(|(_, p)| p)
    }
}

/// ZIF pin under package pin `pin` (1..=8) of an 8-pin chip whose pin 1 sits
/// at ZIF pin `row` (pin 1 towards the top of the socket).
pub fn zif_pin_8(row: u8, pin: u8) -> u8 {
    if pin <= 4 {
        row + pin - 1
    } else {
        49 - (row + 8 - pin)
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

    /// Re-initialise the FTDI channel on the open handle (used after the
    /// MCU switched logic images).
    pub fn reinit(&mut self) -> Result<()> {
        self.ftdi.reset()?;
        self.ftdi.set_latency(2)?;
        self.ftdi.set_flow_rts_cts()?;
        self.ftdi.purge()?;
        let _ = self.ftdi.read_available(Duration::from_millis(5));
        Ok(())
    }

    fn sync(&mut self) -> Result<()> {
        self.ftdi.set_bitmode(0xFF, BitMode::Mpsse)?;
        self.ftdi.write(&[0x82, 0x00, 0x40])?;
        std::thread::sleep(Duration::from_millis(3));
        self.ftdi.write(&[0x82, 0xFF, 0x00])?;
        self.ftdi.set_bitmode(0xFF, BitMode::Reset)?;
        std::thread::sleep(Duration::from_millis(50));
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

    /// Rails for the bit-banged (I2C) socket mode at 3.3 V, as used by the
    /// vendor software for 24xx EEPROMs.
    pub fn set_rails_i2c(&mut self) -> Result<()> {
        let mut words: [u16; 20] = [0x0211; 20];
        words[0] = 0x0010;
        words[1] = 0x00C0;
        words[2] = 0x00EA;
        words[15] = 0x0294;
        words[18] = Voltage::V3_3.rail_code();
        words[19] = Voltage::V3_3.rail_code();
        let payload: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        self.command(CMD_SET_RAILS, 0x14, 0x00, PAD)?;
        self.ftdi.write(&payload)?;
        self.ftdi.read_exact(40)?;
        Ok(())
    }

    /// Hand the socket to the FTDI for bit-banged buses (I2C) and switch
    /// the FTDI to MPSSE. `mode` is the routing byte (`00` in captures).
    pub fn enter_bitbang(&mut self, mode: u8) -> Result<()> {
        self.command(CMD_SET_MODE, 0x00, mode, [1, 0, 0, 0, 0])?;
        self.ftdi.set_bitmode(0x00, BitMode::Reset)?;
        self.ftdi.set_bitmode(0x00, BitMode::Mpsse)?;
        std::thread::sleep(Duration::from_millis(50));
        Ok(())
    }

    /// Power the ISP header (3.3 V logic) and hand its lines to MPSSE.
    pub fn enter_isp(&mut self) -> Result<()> {
        let mut words: [u16; 20] = [0x0211; 20];
        words[0] = 0x0010;
        words[1] = Voltage::V3_3.level_code();
        words[2] = 0x00EA;
        words[14] = 0x0000;
        for i in [15, 18, 19] {
            words[i] = Voltage::V3_3.rail_code();
        }
        let payload: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        self.command(CMD_SET_RAILS, 0x14, 0x00, PAD)?;
        self.ftdi.write(&payload)?;
        self.ftdi.read_exact(40)?;
        self.command(CMD_SET_MODE, 0x00, 0x00, [1, 1, 1, 1, 1])?;
        self.ftdi.set_bitmode(0xFF, BitMode::Reset)?;
        self.ftdi.reset()?;
        self.ftdi.set_latency(6)?;
        self.ftdi.set_bitmode(0x00, BitMode::Reset)?;
        self.ftdi.set_bitmode(0x00, BitMode::Mpsse)?;
        std::thread::sleep(Duration::from_millis(50));
        Ok(())
    }

    /// Give the ISP lines back to the MCU and switch the header off.
    pub fn leave_isp(&mut self) -> Result<()> {
        self.ftdi.set_bitmode(0x00, BitMode::Reset)?;
        self.reinit()?;
        self.command(CMD_SET_MODE, 0x00, 0x00, [0, 0, 0, 0, 1])
    }

    /// Power off after `enter_bitbang`.
    pub fn power_off_bitbang(&mut self, mode: u8) -> Result<()> {
        self.ftdi.set_bitmode(0xFF, BitMode::Reset)?;
        self.command_noack(CMD_POWER_OFF, 0x00, mode, [1, 0, 0, 0, 0])?;
        self.ftdi.set_bitmode(0xFF, BitMode::Mpsse)?;
        self.ftdi.write(&[0x82, 0x00, 0x80])?;
        std::thread::sleep(Duration::from_millis(10));
        self.ftdi.write(&[0x82, 0xFF, 0x00])?;
        self.ftdi.set_bitmode(0xFF, BitMode::Reset)?;
        std::thread::sleep(Duration::from_millis(100));
        self.ftdi.purge()?;
        let _ = self.ftdi.read_available(Duration::from_millis(5));
        Ok(())
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

    /// Connect the socket to the FTDI SPI lines (25-series serial flash).
    pub fn enter_spi(&mut self) -> Result<()> {
        self.command(CMD_SET_MODE, 0x00, SPI_MODE, [1, 1, 1, 1, 1])
    }

    /// Switch the chip supply off and release the socket. `spi` must be
    /// true if `enter_spi` was used since the rails were switched on.
    pub fn power_off(&mut self, spi: bool) -> Result<()> {
        if spi {
            self.command_noack(CMD_POWER_OFF, 0x00, SPI_MODE, [1, 1, 1, 1, 1])?;
        } else {
            self.command_noack(CMD_POWER_OFF, 0x00, 0x00, [0, 0, 0, 0, 0x14])?;
        }
        self.ftdi.set_bitmode(0xFF, BitMode::Mpsse)?;
        self.ftdi.write(&[0x82, 0x00, 0x80])?;
        std::thread::sleep(Duration::from_millis(10));
        self.ftdi.write(&[0x82, 0xFF, 0x00])?;
        self.ftdi.set_bitmode(0xFF, BitMode::Reset)?;
        std::thread::sleep(Duration::from_millis(50));
        self.ftdi.purge()?;
        let _ = self.ftdi.read_available(Duration::from_millis(5));
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
        assert_eq!(
            (1..=8).map(|p| zif_pin_8(21, p)).collect::<Vec<_>>(),
            vec![21, 22, 23, 24, 25, 26, 27, 28]
        );
        // W25Q64 seated correctly at the bottom of the socket
        let good = hex("ff ff f1 0f ff ff ff ff f0 0f ff ff ff ff ff 7f ff ff 01 1c 00 00 00 00 02 18 19 00 00 00");
        let r = ContactReport::parse(good).unwrap();
        assert_eq!(r.connected_pins(), vec![21, 22, 23, 24, 25, 26, 27, 28]);
        assert_eq!(
            r.locate_8pin(),
            Some(Placement8 {
                row: 21,
                missing: vec![]
            })
        );
        // same chip moved up one row, chip pin 4 (ZIF 23) without contact
        let mut moved = vec![0xff; 30];
        moved[8..16].copy_from_slice(&0xFFFF_FFFF_FFFF_C7E1u64.to_le_bytes());
        let r = ContactReport::parse(moved).unwrap();
        assert_eq!(r.connected_pins(), vec![20, 21, 22, 26, 27, 28, 29]);
        assert_eq!(
            r.locate_8pin(),
            Some(Placement8 {
                row: 20,
                missing: vec![4]
            })
        );
        // chip pins 1 and 5 not touching
        let poor = hex("ff ff f1 1f ff ff ff ff f1 1f ff ff ff ff ff 7f ff ff 01 1c 00 00 00 00 02 18 19 00 00 00");
        let r = ContactReport::parse(poor).unwrap();
        assert_eq!(r.locate_8pin().unwrap().missing, vec![1, 5]);
        assert!(ContactReport::parse(vec![0xff; 48]).unwrap().socket_empty());
    }

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace()
            .map(|x| u8::from_str_radix(x, 16).unwrap())
            .collect()
    }
}
