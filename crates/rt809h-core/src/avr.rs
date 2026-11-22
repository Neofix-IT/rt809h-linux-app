//! Atmel/Microchip AVR microcontrollers over the ISP header (SPI serial
//! programming, as described in the "Serial Downloading" chapter of the AVR
//! datasheets).
//!
//! The programmer routes its FTDI MPSSE lines to the ISP header:
//! ADBUS0 = SCK, ADBUS1 = MOSI, ADBUS2 = MISO, ADBUS3 = RESET.
//! Every instruction is four bytes; the answer is the last byte shifted in.

use std::time::{Duration, Instant};

use crate::ftdi::Ftdi;
use crate::spi::Progress;
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AvrPart {
    pub name: &'static str,
    pub signature: [u8; 3],
    /// flash bytes
    pub flash: u32,
    /// flash page bytes
    pub page: u16,
    /// EEPROM bytes
    pub eeprom: u16,
    /// number of fuse bytes (2 = low/high, 3 = low/high/extended)
    pub fuses: u8,
}

const fn p(name: &'static str, sig: u32, flash: u32, page: u16, eeprom: u16, fuses: u8) -> AvrPart {
    AvrPart {
        name,
        signature: [(sig >> 16) as u8, (sig >> 8) as u8, sig as u8],
        flash,
        page,
        eeprom,
        fuses,
    }
}

/// Parts with ISP serial programming, from the device datasheets.
pub static PARTS: &[AvrPart] = &[
    p("ATtiny13", 0x1E9007, 1024, 32, 64, 2),
    p("ATtiny13A", 0x1E9007, 1024, 32, 64, 2),
    p("ATtiny24", 0x1E910B, 2048, 32, 128, 3),
    p("ATtiny24A", 0x1E910B, 2048, 32, 128, 3),
    p("ATtiny44", 0x1E9207, 4096, 64, 256, 3),
    p("ATtiny44A", 0x1E9207, 4096, 64, 256, 3),
    p("ATtiny84", 0x1E930C, 8192, 64, 512, 3),
    p("ATtiny84A", 0x1E930C, 8192, 64, 512, 3),
    p("ATtiny25", 0x1E9108, 2048, 32, 128, 3),
    p("ATtiny45", 0x1E9206, 4096, 64, 256, 3),
    p("ATtiny85", 0x1E930B, 8192, 64, 512, 3),
    p("ATtiny26", 0x1E9109, 2048, 32, 128, 2),
    p("ATtiny261", 0x1E910C, 2048, 32, 128, 3),
    p("ATtiny261A", 0x1E910C, 2048, 32, 128, 3),
    p("ATtiny461", 0x1E9208, 4096, 64, 256, 3),
    p("ATtiny461A", 0x1E9208, 4096, 64, 256, 3),
    p("ATtiny861", 0x1E930D, 8192, 64, 512, 3),
    p("ATtiny861A", 0x1E930D, 8192, 64, 512, 3),
    p("ATtiny2313", 0x1E910A, 2048, 32, 128, 3),
    p("ATtiny2313A", 0x1E910A, 2048, 32, 128, 3),
    p("ATtiny4313", 0x1E920D, 4096, 64, 256, 3),
    p("ATtiny48", 0x1E9209, 4096, 64, 64, 3),
    p("ATtiny88", 0x1E9311, 8192, 64, 64, 3),
    p("ATtiny87", 0x1E9387, 8192, 128, 512, 3),
    p("ATtiny167", 0x1E9487, 16384, 128, 512, 3),
    p("ATtiny1634", 0x1E9412, 16384, 32, 256, 3),
    p("ATmega8", 0x1E9307, 8192, 64, 512, 2),
    p("ATmega8A", 0x1E9307, 8192, 64, 512, 2),
    p("ATmega8515", 0x1E9306, 8192, 64, 512, 2),
    p("ATmega8535", 0x1E9308, 8192, 64, 512, 2),
    p("ATmega16", 0x1E9403, 16384, 128, 512, 2),
    p("ATmega16A", 0x1E9403, 16384, 128, 512, 2),
    p("ATmega32", 0x1E9502, 32768, 128, 1024, 2),
    p("ATmega32A", 0x1E9502, 32768, 128, 1024, 2),
    p("ATmega64", 0x1E9602, 65536, 256, 2048, 3),
    p("ATmega64A", 0x1E9602, 65536, 256, 2048, 3),
    p("ATmega128", 0x1E9702, 131072, 256, 4096, 3),
    p("ATmega128A", 0x1E9702, 131072, 256, 4096, 3),
    p("ATmega162", 0x1E9404, 16384, 128, 512, 3),
    p("ATmega169", 0x1E9405, 16384, 128, 512, 3),
    p("ATmega169A", 0x1E9405, 16384, 128, 512, 3),
    p("ATmega169P", 0x1E9405, 16384, 128, 512, 3),
    p("ATmega169PA", 0x1E9405, 16384, 128, 512, 3),
    p("ATmega48", 0x1E9205, 4096, 64, 256, 3),
    p("ATmega48A", 0x1E9205, 4096, 64, 256, 3),
    p("ATmega48P", 0x1E920A, 4096, 64, 256, 3),
    p("ATmega48PA", 0x1E920A, 4096, 64, 256, 3),
    p("ATmega88", 0x1E930A, 8192, 64, 512, 3),
    p("ATmega88A", 0x1E930A, 8192, 64, 512, 3),
    p("ATmega88P", 0x1E930F, 8192, 64, 512, 3),
    p("ATmega88PA", 0x1E930F, 8192, 64, 512, 3),
    p("ATmega168", 0x1E9406, 16384, 128, 512, 3),
    p("ATmega168A", 0x1E9406, 16384, 128, 512, 3),
    p("ATmega168P", 0x1E940B, 16384, 128, 512, 3),
    p("ATmega168PA", 0x1E940B, 16384, 128, 512, 3),
    p("ATmega328", 0x1E9514, 32768, 128, 1024, 3),
    p("ATmega328P", 0x1E950F, 32768, 128, 1024, 3),
    p("ATmega328PB", 0x1E9516, 32768, 128, 1024, 3),
    p("ATmega164A", 0x1E940F, 16384, 128, 512, 3),
    p("ATmega164P", 0x1E940A, 16384, 128, 512, 3),
    p("ATmega164PA", 0x1E940A, 16384, 128, 512, 3),
    p("ATmega324A", 0x1E9515, 32768, 128, 1024, 3),
    p("ATmega324P", 0x1E9508, 32768, 128, 1024, 3),
    p("ATmega324PA", 0x1E9511, 32768, 128, 1024, 3),
    p("ATmega644", 0x1E9609, 65536, 256, 2048, 3),
    p("ATmega644A", 0x1E9609, 65536, 256, 2048, 3),
    p("ATmega644P", 0x1E960A, 65536, 256, 2048, 3),
    p("ATmega644PA", 0x1E960A, 65536, 256, 2048, 3),
    p("ATmega1284", 0x1E9706, 131072, 256, 4096, 3),
    p("ATmega1284P", 0x1E9705, 131072, 256, 4096, 3),
    p("ATmega640", 0x1E9608, 65536, 256, 4096, 3),
    p("ATmega1280", 0x1E9703, 131072, 256, 4096, 3),
    p("ATmega1281", 0x1E9704, 131072, 256, 4096, 3),
    p("ATmega2560", 0x1E9801, 262144, 256, 4096, 3),
    p("ATmega2561", 0x1E9802, 262144, 256, 4096, 3),
    p("ATmega8U2", 0x1E9389, 8192, 64, 512, 3),
    p("ATmega16U2", 0x1E9489, 16384, 128, 512, 3),
    p("ATmega32U2", 0x1E958A, 32768, 128, 1024, 3),
    p("ATmega16U4", 0x1E9488, 16384, 128, 512, 3),
    p("ATmega32U4", 0x1E9587, 32768, 128, 1024, 3),
    p("AT90USB646", 0x1E9682, 65536, 256, 2048, 3),
    p("AT90USB647", 0x1E9682, 65536, 256, 2048, 3),
    p("AT90USB1286", 0x1E9782, 131072, 256, 4096, 3),
    p("AT90USB1287", 0x1E9782, 131072, 256, 4096, 3),
];

/// Part by name (case-insensitive; a package suffix after `@` is ignored).
pub fn find(name: &str) -> Option<&'static AvrPart> {
    let base = name.split('@').next().unwrap_or(name).trim();
    PARTS.iter().find(|p| p.name.eq_ignore_ascii_case(base))
}

/// Names of the parts with this signature.
pub fn by_signature(sig: [u8; 3]) -> Vec<&'static str> {
    PARTS
        .iter()
        .filter(|p| p.signature == sig)
        .map(|p| p.name)
        .collect()
}

/// Which memory of the part a read/write works on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Memory {
    #[default]
    Flash,
    Eeprom,
}

/// Fuse and lock bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fuses {
    pub low: u8,
    pub high: u8,
    pub extended: Option<u8>,
    pub lock: u8,
}

impl std::fmt::Display for Fuses {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "low 0x{:02X}, high 0x{:02X}", self.low, self.high)?;
        if let Some(e) = self.extended {
            write!(f, ", extended 0x{e:02X}")?;
        }
        write!(f, ", lock 0x{:02X}", self.lock)
    }
}

// MPSSE opcodes: bytes out on the falling edge, in on the rising edge, MSB first
const OUT: u8 = 0x11;
const IN: u8 = 0x20;
const INOUT: u8 = 0x31;
const SEND_IMMEDIATE: u8 = 0x87;
const GPIO_LOW: u8 = 0x80;
const CLK_DIV: u8 = 0x86;
/// SCK, MOSI and RESET are outputs
const DIR: u8 = 0x0B;
const MOSI: u8 = 0x02;
const RESET: u8 = 0x08;

/// SCK dividers tried from fast to slow (12 MHz / (2 × (div + 1))):
/// 1.5 MHz, 750 kHz, 400 kHz, 300 kHz, 100 kHz, 40 kHz.
const DIVIDERS: [u16; 6] = [0x03, 0x07, 0x0E, 0x13, 0x3B, 0x95];

/// Words read per USB batch.
const READ_BATCH: u32 = 4096;

pub struct Isp<'a> {
    ftdi: &'a mut Ftdi,
    div: u16,
}

impl<'a> Isp<'a> {
    /// Enter programming mode, slowing the clock until the part answers and
    /// reads a plausible signature. Data transfers run one step slower than
    /// the fastest clock that worked, which leaves margin for parts running
    /// from a slow clock.
    pub fn enter(ftdi: &'a mut Ftdi) -> Result<Self> {
        let mut isp = Isp { ftdi, div: 0 };
        for (i, &div) in DIVIDERS.iter().enumerate() {
            isp.div = div;
            if isp.sync()? && plausible(isp.signature()?) {
                let slower = DIVIDERS[(i + 1).min(DIVIDERS.len() - 1)];
                if slower != div {
                    isp.div = slower;
                    if !isp.sync()? {
                        isp.div = div;
                        isp.sync()?;
                    }
                }
                log::debug!("ISP clock divider 0x{:02X}", isp.div);
                return Ok(isp);
            }
        }
        let _ = isp.release();
        Err(Error::Chip(
            "The microcontroller does not answer on the ISP header. Check the cable, that \
             the target is powered and that it has a running clock."
                .into(),
        ))
    }

    /// SCK frequency in Hz.
    pub fn clock_hz(&self) -> u32 {
        12_000_000 / (2 * (self.div as u32 + 1))
    }

    fn gpio(&mut self, value: u8) -> Result<()> {
        self.ftdi.write(&[GPIO_LOW, value, DIR])
    }

    /// Pulse RESET with SCK low, then send Programming Enable; true if the
    /// part echoes it.
    fn sync(&mut self) -> Result<bool> {
        let d = self.div;
        self.ftdi.write(&[CLK_DIV, d as u8, (d >> 8) as u8])?;
        for _ in 0..2 {
            self.gpio(0)?;
            std::thread::sleep(Duration::from_millis(10));
            self.gpio(RESET)?;
            std::thread::sleep(Duration::from_millis(10));
            self.gpio(MOSI)?;
            std::thread::sleep(Duration::from_millis(25));
            let r = self.exchange([0xAC, 0x53, 0x00, 0x00])?;
            if r[2] == 0x53 {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Send one instruction and return the four bytes shifted in.
    fn exchange(&mut self, ins: [u8; 4]) -> Result<[u8; 4]> {
        let mut cmd = vec![INOUT, 0x03, 0x00];
        cmd.extend_from_slice(&ins);
        cmd.push(SEND_IMMEDIATE);
        self.ftdi.write(&cmd)?;
        let r = self.ftdi.read_exact(4)?;
        Ok([r[0], r[1], r[2], r[3]])
    }

    fn query(&mut self, a: u8, b: u8, c: u8) -> Result<u8> {
        Ok(self.exchange([a, b, c, 0])?[3])
    }

    pub fn signature(&mut self) -> Result<[u8; 3]> {
        Ok([
            self.query(0x30, 0, 0)?,
            self.query(0x30, 0, 1)?,
            self.query(0x30, 0, 2)?,
        ])
    }

    pub fn fuses(&mut self, count: u8) -> Result<Fuses> {
        Ok(Fuses {
            low: self.query(0x50, 0x00, 0)?,
            high: self.query(0x58, 0x08, 0)?,
            extended: if count > 2 {
                Some(self.query(0x50, 0x08, 0)?)
            } else {
                None
            },
            lock: self.query(0x58, 0x00, 0)?,
        })
    }

    /// Program the fuse bytes that are given.
    pub fn write_fuses(
        &mut self,
        low: Option<u8>,
        high: Option<u8>,
        extended: Option<u8>,
        lock: Option<u8>,
    ) -> Result<()> {
        for (op, v) in [(0xA0, low), (0xA8, high), (0xA4, extended), (0xE0, lock)] {
            if let Some(v) = v {
                self.exchange([0xAC, op, 0x00, v])?;
                self.wait_ready(Duration::from_millis(50))?;
            }
        }
        Ok(())
    }

    /// Poll RDY/BSY until the part is idle.
    fn wait_ready(&mut self, max: Duration) -> Result<()> {
        let start = Instant::now();
        loop {
            if self.query(0xF0, 0, 0)? & 1 == 0 {
                return Ok(());
            }
            if start.elapsed() > max {
                return Err(Error::Timeout("the microcontroller stays busy".into()));
            }
            std::thread::sleep(Duration::from_micros(500));
        }
    }

    /// Erase flash, EEPROM (unless the EESAVE fuse is set) and lock bits.
    pub fn chip_erase(&mut self) -> Result<()> {
        self.exchange([0xAC, 0x80, 0x00, 0x00])?;
        std::thread::sleep(Duration::from_millis(20));
        self.wait_ready(Duration::from_millis(500))?;
        // re-enter programming mode so the erase takes effect everywhere
        if !self.sync()? {
            return Err(Error::Chip("Lost the microcontroller after erasing".into()));
        }
        Ok(())
    }

    fn extended_address(&mut self, word: u32) -> Result<()> {
        self.ftdi
            .write(&[OUT, 0x03, 0x00, 0x4D, 0x00, (word >> 16) as u8, 0x00])
    }

    /// Read `len` bytes of flash.
    pub fn read_flash(&mut self, part: &AvrPart, progress: Progress) -> Result<Vec<u8>> {
        let words = part.flash / 2;
        let mut out = Vec::with_capacity(part.flash as usize);
        let mut w = 0;
        while w < words {
            let n = READ_BATCH.min(words - w);
            if part.flash > 0x20000 {
                self.extended_address(w)?;
            }
            let mut cmd = Vec::with_capacity(n as usize * 18 + 1);
            for a in w..w + n {
                for op in [0x20, 0x28] {
                    cmd.extend_from_slice(&[OUT, 0x02, 0x00, op, (a >> 8) as u8, a as u8]);
                    cmd.extend_from_slice(&[IN, 0x00, 0x00]);
                }
            }
            cmd.push(SEND_IMMEDIATE);
            self.ftdi.write(&cmd)?;
            out.extend(self.ftdi.read_exact(n as usize * 2)?);
            w += n;
            if !progress(out.len() as u64, part.flash as u64) {
                return Err(Error::Cancelled);
            }
        }
        Ok(out)
    }

    /// Read the whole EEPROM.
    pub fn read_eeprom(&mut self, part: &AvrPart, progress: Progress) -> Result<Vec<u8>> {
        let mut cmd = Vec::with_capacity(part.eeprom as usize * 9 + 1);
        for a in 0..part.eeprom {
            cmd.extend_from_slice(&[OUT, 0x02, 0x00, 0xA0, (a >> 8) as u8, a as u8]);
            cmd.extend_from_slice(&[IN, 0x00, 0x00]);
        }
        cmd.push(SEND_IMMEDIATE);
        self.ftdi.write(&cmd)?;
        let d = self.ftdi.read_exact(part.eeprom as usize)?;
        progress(d.len() as u64, d.len() as u64);
        Ok(d)
    }

    /// Program flash page by page; the part must be erased. Pages that are
    /// all `FF` are skipped.
    pub fn write_flash(&mut self, part: &AvrPart, data: &[u8], progress: Progress) -> Result<()> {
        let page = part.page as usize;
        for (i, chunk) in data.chunks(page).enumerate() {
            let base = (i * page / 2) as u32;
            if chunk.iter().any(|&b| b != 0xFF) {
                let mut cmd = Vec::with_capacity(page * 7);
                for (j, &b) in chunk.iter().enumerate() {
                    let op = if j % 2 == 0 { 0x40 } else { 0x48 };
                    cmd.extend_from_slice(&[OUT, 0x03, 0x00, op, 0x00, (j / 2) as u8, b]);
                }
                if part.flash > 0x20000 {
                    self.extended_address(base)?;
                }
                cmd.extend_from_slice(&[OUT, 0x03, 0x00, 0x4C, (base >> 8) as u8, base as u8, 0]);
                self.ftdi.write(&cmd)?;
                self.wait_ready(Duration::from_millis(50))?;
            }
            if !progress(((i + 1) * page).min(data.len()) as u64, data.len() as u64) {
                return Err(Error::Cancelled);
            }
        }
        Ok(())
    }

    /// Write the EEPROM bytes that differ from `old`.
    pub fn write_eeprom(&mut self, data: &[u8], old: &[u8], progress: Progress) -> Result<()> {
        for (a, (&b, &o)) in data.iter().zip(old).enumerate() {
            if b != o {
                self.exchange([0xC0, (a >> 8) as u8, a as u8, b])?;
                self.wait_ready(Duration::from_millis(20))?;
            }
            if a % 64 == 63 && !progress(a as u64 + 1, data.len() as u64) {
                return Err(Error::Cancelled);
            }
        }
        progress(data.len() as u64, data.len() as u64);
        Ok(())
    }

    /// Let the part run again (RESET high).
    pub fn release(&mut self) -> Result<()> {
        self.gpio(RESET)
    }
}

fn plausible(sig: [u8; 3]) -> bool {
    sig[0] == 0x1E && sig != [0x1E, 0xFF, 0xFF] && sig != [0x1E, 0x00, 0x00]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table() {
        let p = find("ATMEGA328P@DIP28").unwrap();
        assert_eq!(p.signature, [0x1E, 0x95, 0x0F]);
        assert_eq!(by_signature([0x1E, 0x93, 0x0B]), vec!["ATtiny85"]);
        for p in PARTS {
            assert_eq!(p.flash % p.page as u32, 0, "{}", p.name);
        }
    }
}
