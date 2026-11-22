//! I2C serial EEPROMs (24xx family), bit-banged through the FTDI MPSSE
//! engine.
//!
//! Pins: ADBUS0 = SCL, ADBUS1 = SDA out, ADBUS2 = SDA in. SDA is released by
//! switching ADBUS1 to an input; the socket has pull-ups. Bus speed 100 kHz.

use crate::ftdi::Ftdi;
use crate::spi::Progress;
use crate::{Error, Result};

const DIR_DRIVE: u8 = 0x13;
const DIR_RELEASE: u8 = 0x11;
const DIR_IDLE: u8 = 0x10;
/// 12 MHz / ((1 + 0x3B) * 2) = 100 kHz
const DIVISOR: [u8; 3] = [0x86, 0x3B, 0x00];

/// Size-dependent addressing of a 24xx part.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EepromGeometry {
    pub size: u32,
    /// word address bytes after the device address (1 or 2)
    pub addr_bytes: u8,
    pub page: u32,
}

impl EepromGeometry {
    pub fn for_size(size: u32) -> Self {
        let (addr_bytes, page) = match size {
            0..=256 => (1, 8),
            257..=2048 => (1, 16),
            2049..=8192 => (2, 32),
            8193..=32768 => (2, 64),
            32769..=65536 => (2, 128),
            _ => (2, 256),
        };
        EepromGeometry {
            size,
            addr_bytes,
            page,
        }
    }

    /// Device address byte (write) for a memory address: upper address bits
    /// that do not fit the word address go into A0..A2.
    fn device(&self, addr: u32) -> u8 {
        let span = if self.addr_bytes == 1 { 8 } else { 16 };
        0xA0 | (((addr >> span) & 7) as u8) << 1
    }

    fn word(&self, addr: u32) -> Vec<u8> {
        if self.addr_bytes == 1 {
            vec![addr as u8]
        } else {
            vec![(addr >> 8) as u8, addr as u8]
        }
    }

    /// Largest block that can be read without crossing a device address
    /// boundary.
    fn read_block(&self) -> u32 {
        if self.addr_bytes == 1 {
            256
        } else {
            65536
        }
    }
}

/// MPSSE command builder for bit-banged I2C.
#[derive(Default)]
struct Cmd(Vec<u8>);

impl Cmd {
    fn pins(&mut self, scl: bool, sda: bool, dir: u8, repeat: usize) {
        let v = scl as u8 | (sda as u8) << 1;
        for _ in 0..repeat {
            self.0.extend_from_slice(&[0x80, v, dir]);
        }
    }

    fn start(&mut self) {
        self.pins(true, true, DIR_DRIVE, 4);
        self.pins(true, false, DIR_DRIVE, 8);
        self.pins(false, false, DIR_DRIVE, 4);
    }

    fn stop(&mut self) {
        self.pins(false, true, DIR_DRIVE, 1);
        self.pins(false, false, DIR_DRIVE, 1);
        self.pins(true, false, DIR_DRIVE, 4);
        self.pins(true, true, DIR_DRIVE, 8);
        self.pins(false, false, DIR_IDLE, 1);
    }

    /// Send a byte and clock in the ACK bit (1 reply byte, bit 0 = NACK).
    fn byte_ack(&mut self, b: u8) {
        self.pins(false, true, DIR_DRIVE, 1);
        self.0.extend_from_slice(&[0x11, 0x00, 0x00, b]);
        self.pins(false, true, DIR_RELEASE, 3);
        self.0.extend_from_slice(&[0x22, 0x00]);
    }

    /// Send a byte and clock past the ACK without reading it.
    fn byte(&mut self, b: u8) {
        self.pins(false, true, DIR_DRIVE, 1);
        self.0.extend_from_slice(&[0x11, 0x00, 0x00, b, 0x8E, 0x00]);
    }

    /// Read a byte (1 reply byte) and answer ACK, or NACK for the last one.
    fn read(&mut self, last: bool) {
        self.pins(false, true, DIR_DRIVE, 2);
        self.0.extend_from_slice(&[0x24, 0x00, 0x00]);
        self.pins(false, last, DIR_DRIVE, 1);
        self.0.extend_from_slice(&[0x8E, 0x00]);
    }

    fn flush(&mut self) {
        self.0.push(0x87);
    }
}

/// Byte pipe to the MPSSE engine (the FTDI, or a chip emulator in tests).
pub trait Link {
    fn write(&mut self, data: &[u8]) -> Result<()>;
    fn read_exact(&mut self, n: usize) -> Result<Vec<u8>>;
}

impl Link for Ftdi {
    fn write(&mut self, data: &[u8]) -> Result<()> {
        Ftdi::write(self, data)
    }
    fn read_exact(&mut self, n: usize) -> Result<Vec<u8>> {
        Ftdi::read_exact(self, n)
    }
}

impl Link for crate::emu::Mpsse {
    fn write(&mut self, data: &[u8]) -> Result<()> {
        self.feed(data);
        Ok(())
    }
    fn read_exact(&mut self, n: usize) -> Result<Vec<u8>> {
        if self.replies.len() < n {
            return Err(Error::Timeout(format!(
                "emulator has {} of {n} bytes",
                self.replies.len()
            )));
        }
        Ok(self.replies.drain(..n).collect())
    }
}

pub struct I2cBus<'a> {
    ftdi: &'a mut dyn Link,
}

impl<'a> I2cBus<'a> {
    /// The FTDI must already be in MPSSE mode with the socket routed.
    pub fn init(ftdi: &'a mut dyn Link) -> Result<Self> {
        ftdi.write(&DIVISOR)?;
        let mut c = Cmd::default();
        c.pins(true, true, DIR_DRIVE, 2);
        c.stop();
        ftdi.write(&c.0)?;
        Ok(I2cBus { ftdi })
    }

    fn run(&mut self, mut c: Cmd, replies: usize) -> Result<Vec<u8>> {
        if replies == 0 {
            return self.ftdi.write(&c.0).map(|_| Vec::new());
        }
        c.flush();
        self.ftdi.write(&c.0)?;
        self.ftdi.read_exact(replies)
    }

    /// True if a device acknowledges `dev` (8-bit write address).
    pub fn probe(&mut self, dev: u8) -> Result<bool> {
        let mut c = Cmd::default();
        c.start();
        c.byte_ack(dev);
        c.stop();
        Ok(self.run(c, 1)?[0] & 1 == 0)
    }

    /// Wait for the end of an internal write cycle (acknowledge polling).
    fn wait_ready(&mut self, dev: u8) -> Result<()> {
        for _ in 0..200 {
            if self.probe(dev)? {
                return Ok(());
            }
        }
        Err(Error::Protocol(
            "EEPROM did not finish its write cycle".into(),
        ))
    }
}

pub struct Eeprom<'a> {
    bus: I2cBus<'a>,
    pub geo: EepromGeometry,
}

impl<'a> Eeprom<'a> {
    pub fn new(bus: I2cBus<'a>, geo: EepromGeometry) -> Self {
        Eeprom { bus, geo }
    }

    /// Check that the chip answers at its base address.
    pub fn present(&mut self) -> Result<bool> {
        self.bus.probe(self.geo.device(0))
    }

    pub fn read(&mut self, progress: Progress) -> Result<Vec<u8>> {
        let size = self.geo.size;
        let mut out = Vec::with_capacity(size as usize);
        // keep each USB transfer modest: 256 bytes per command batch
        let chunk = 256.min(self.geo.read_block()).min(size);
        let mut addr = 0;
        while addr < size {
            let n = chunk.min(size - addr);
            let dev = self.geo.device(addr);
            let mut c = Cmd::default();
            c.start();
            c.byte_ack(dev);
            for w in self.geo.word(addr) {
                c.byte_ack(w);
            }
            c.start();
            c.byte_ack(dev | 1);
            for i in 0..n {
                c.read(i + 1 == n);
            }
            c.stop();
            let acks = 2 + self.geo.addr_bytes as usize;
            let r = self.bus.run(c, acks + n as usize)?;
            if r[..acks].iter().any(|a| a & 1 != 0) {
                return Err(Error::Protocol(format!(
                    "no acknowledge from the EEPROM at 0x{addr:X}"
                )));
            }
            out.extend_from_slice(&r[acks..]);
            addr += n;
            if !progress(addr as u64, size as u64) {
                return Err(Error::Cancelled);
            }
        }
        Ok(out)
    }

    pub fn write(&mut self, data: &[u8], progress: Progress) -> Result<()> {
        let size = (data.len() as u32).min(self.geo.size);
        let page = self.geo.page;
        let mut addr = 0;
        while addr < size {
            let n = (page - addr % page).min(size - addr);
            let dev = self.geo.device(addr);
            self.bus.wait_ready(dev)?;
            let mut c = Cmd::default();
            c.start();
            c.byte(dev);
            for w in self.geo.word(addr) {
                c.byte(w);
            }
            for &b in &data[addr as usize..(addr + n) as usize] {
                c.byte(b);
            }
            c.stop();
            self.bus.run(c, 0)?;
            addr += n;
            if !progress(addr as u64, size as u64) {
                return Err(Error::Cancelled);
            }
        }
        self.bus.wait_ready(self.geo.device(0))
    }

    /// EEPROMs have no erase command: fill with 0xFF.
    pub fn erase(&mut self, progress: Progress) -> Result<()> {
        let ff = vec![0xFF; self.geo.size as usize];
        self.write(&ff, progress)
    }
}

/// Capacity in bytes from a 24xx part number ("AT24C02D" -> 256,
/// "24LC256" -> 32768, "M24M01" -> 131072). None if it does not look like
/// a 24-series part.
pub fn size_from_name(name: &str) -> Option<u32> {
    let base = name.split(['@', '(']).next()?.to_ascii_uppercase();
    // kbit-numbered serial memories (FRAM, nvSRAM)
    for key in ["85RC", "14MB", "14ME", "14MC"] {
        if let Some(i) = base.find(key) {
            let d: String = base[i + 4..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if let Some(k) = d.parse::<u32>().ok().filter(|k| k.is_power_of_two()) {
                return Some(k * 128);
            }
        }
    }
    // 24xx EEPROMs, the 34xx SPD variants and ST's 25Cxx/25Wxx I2C parts
    let b = base.as_bytes();
    (0..b.len().saturating_sub(1))
        .filter(|&i| {
            let next = b.get(i + 2).copied().unwrap_or(b' ');
            match &b[i..i + 2] {
                b"24" | b"34" => true,
                b"25" => matches!(next, b'C' | b'W' | b'E'),
                _ => false,
            }
        })
        .find_map(|i| series_size(&base[i + 2..]))
}

/// Capacity from what follows the series number ("C02" -> 256, "LC512" -> 64 KiB).
pub(crate) fn series_size(rest: &str) -> Option<u32> {
    // skip family letters (C, LC, AA, CS, FC, CM, ...)
    let letters: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    if letters.len() > 3 {
        return None;
    }
    let digits: String = rest[letters.len()..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    let after = rest[letters.len() + digits.len()..].chars().next();
    if (letters.ends_with('M') || after == Some('M')) && !digits.is_empty() {
        // 24M01, 24CM02, BR24G1M: megabits
        return digits.parse::<u32>().ok().map(|m| m * 128 * 1024);
    }
    let kbit = |d: &str| -> Option<u32> {
        let k: u32 = d.parse().ok()?;
        (d.len() >= 2 && k.is_power_of_two() && (1..=512).contains(&k)).then_some(k * 128)
    };
    match digits.as_str() {
        "00" => Some(16),
        "21" | "014" => Some(128),
        "52" => Some(256),
        "024" | "025" | "03" => Some(256),
        "05" => Some(512),
        "17" | "164" | "174" => Some(2048),
        "65" | "66" => Some(8192),
        "1024" | "1025" | "1026" => Some(128 * 1024),
        "2048" => Some(256 * 1024),
        d => kbit(d).or_else(|| {
            // trailing variant digit: 24C023, 25C086, 24W166
            (d.len() == 3).then(|| kbit(&d[..2])).flatten()
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(size_from_name("AT24C02D@SOIC8-150"), Some(256));
        assert_eq!(size_from_name("24LC256"), Some(32768));
        assert_eq!(size_from_name("M24M01"), Some(131072));
        assert_eq!(size_from_name("24C1024"), Some(131072));
        assert_eq!(size_from_name("BR24G16"), Some(2048));
        assert_eq!(size_from_name("24AA00"), Some(16));
        assert_eq!(size_from_name("24C21"), Some(128));
        assert_eq!(size_from_name("W25Q64"), None);
        assert_eq!(size_from_name("24LC025"), Some(256));
        assert_eq!(size_from_name("34AA04"), Some(512));
        assert_eq!(size_from_name("CAT24FC65"), Some(8192));
        assert_eq!(size_from_name("ST25C086"), Some(1024));
        assert_eq!(size_from_name("LE24C023"), Some(256));
        assert_eq!(size_from_name("CAT24WC17"), Some(2048));
        assert_eq!(size_from_name("MB85RC64TA"), Some(8192));
        assert_eq!(size_from_name("CY14MB064J1"), Some(8192));
        assert_eq!(size_from_name("S524AD0XD1"), None);
        assert_eq!(size_from_name("BR24G1M-3A"), Some(131072));
        assert_eq!(size_from_name("24LCS52"), Some(256));
    }

    fn roundtrip(size: u32) {
        let mut emu = crate::emu::Mpsse::new(Box::new(crate::emu::I2cEeprom::new(size as usize)));
        let bus = I2cBus::init(&mut emu).unwrap();
        let mut e = Eeprom::new(bus, EepromGeometry::for_size(size));
        assert!(e.present().unwrap());
        let mut p = |_: u64, _: u64| true;
        let got = e.read(&mut p).unwrap();
        let want: Vec<u8> = (0..size as usize).map(crate::emu::pattern).collect();
        assert_eq!(got, want, "read {size}");
        let data: Vec<u8> = (0..size).map(|i| (i * 7 + 3) as u8).collect();
        e.write(&data, &mut p).unwrap();
        assert_eq!(e.read(&mut p).unwrap(), data, "write {size}");
    }

    #[test]
    fn emulated_chips() {
        for size in [16, 128, 256, 512, 2048, 8192, 32768, 131072] {
            roundtrip(size);
        }
    }

    #[test]
    fn addressing() {
        let g = EepromGeometry::for_size(2048);
        assert_eq!(g.device(0x512), 0xAA);
        assert_eq!(g.word(0x512), vec![0x12]);
        let g = EepromGeometry::for_size(128 * 1024);
        assert_eq!(g.device(0x1_2345), 0xA2);
        assert_eq!(g.word(0x1_2345), vec![0x23, 0x45]);
    }
}
