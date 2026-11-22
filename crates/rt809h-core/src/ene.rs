//! ENE KB9012 laptop embedded controller, programmed in circuit over its
//! EDI (ENE debug interface) pins.
//!
//! EDI is an SPI-like bus (mode 0, MSB first) that gives access to the EC's
//! internal register space:
//! - write: `40 addr_hi addr_lo value`
//! - read:  `30 addr_hi addr_lo`, then bytes are clocked in: `5F` while the
//!   EC is not ready, `50` once it is, followed by the value
//! - `F3` leaves EDI mode
//!
//! The internal flash is reached through the embedded flash controller
//! registers (address, data, command, configuration). The 8051 core is held
//! in reset while the flash is accessed.
//!
//! Wiring (same contacts as the ISP header use): EDI CS, CLK, DI, DO, GND.

use std::time::{Duration, Instant};

use crate::ftdi::Ftdi;
use crate::spi::Progress;
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnePart {
    pub name: &'static str,
    /// value of the hardware version register
    pub hw_version: u8,
    /// value of the EDI ID register
    pub edi_id: u8,
    pub size: u32,
    /// program page
    pub page: u32,
    /// erase sector
    pub sector: u32,
}

pub static PARTS: &[EnePart] = &[EnePart {
    name: "KB9012",
    hw_version: 0xC3,
    edi_id: 0x04,
    size: 128 * 1024,
    page: 128,
    sector: 128,
}];

pub fn find(name: &str) -> Option<&'static EnePart> {
    let base = name.split(['@', ' ']).next().unwrap_or(name);
    PARTS.iter().find(|p| p.name.eq_ignore_ascii_case(base))
}

// EDI commands and answers
const EDI_READ: u8 = 0x30;
const EDI_WRITE: u8 = 0x40;
const EDI_DISABLE: u8 = 0xF3;
const EDI_READY: u8 = 0x50;
const EDI_NOT_READY: u8 = 0x5F;

// registers
const HW_VERSION: u16 = 0xFF00;
const PXCFG: u16 = 0xFF14;
const PXCFG_8051_RESET: u8 = 0x01;
const EDI_ID: u16 = 0xFF24;
const EFA0: u16 = 0xFEA8;
const EFA1: u16 = 0xFEA9;
const EFA2: u16 = 0xFEAA;
const EFDAT: u16 = 0xFEAB;
const EFCMD: u16 = 0xFEAC;
const EFCFG: u16 = 0xFEAD;
const EFCFG_WE: u8 = 0x08;
const EFCFG_BUSY: u8 = 0x02;

// flash controller commands
const FL_LATCH: u8 = 0x02;
const FL_READ: u8 = 0x03;
const FL_ERASE: u8 = 0x20;
const FL_PROGRAM: u8 = 0x70;
const FL_CLEAR_LATCHES: u8 = 0x80;

// MPSSE: ADBUS0 = CLK, 1 = DI (to the EC), 2 = DO (from the EC), 3 = CS#
const GPIO_LOW: u8 = 0x80;
const DIR: u8 = 0x0B;
const CS_HIGH: u8 = 0x08;
const OUT: u8 = 0x11;
const IN: u8 = 0x20;
const SEND_IMMEDIATE: u8 = 0x87;
/// 12 MHz / (2 × (div + 1)) = 1 MHz
const CLK_DIV: u8 = 5;
/// bytes clocked in after a read command
const READ_WINDOW: usize = 4;
/// flash bytes per USB batch when reading
const READ_BATCH: u32 = 256;

pub struct Edi<'a> {
    ftdi: &'a mut Ftdi,
    pxcfg: u8,
}

fn push_write(cmd: &mut Vec<u8>, addr: u16, value: u8) {
    cmd.extend_from_slice(&[GPIO_LOW, 0x00, DIR]);
    cmd.extend_from_slice(&[
        OUT,
        0x03,
        0x00,
        EDI_WRITE,
        (addr >> 8) as u8,
        addr as u8,
        value,
    ]);
    cmd.extend_from_slice(&[GPIO_LOW, CS_HIGH, DIR]);
}

fn push_read(cmd: &mut Vec<u8>, addr: u16) {
    cmd.extend_from_slice(&[GPIO_LOW, 0x00, DIR]);
    cmd.extend_from_slice(&[OUT, 0x02, 0x00, EDI_READ, (addr >> 8) as u8, addr as u8]);
    cmd.extend_from_slice(&[IN, (READ_WINDOW - 1) as u8, 0x00]);
    cmd.extend_from_slice(&[GPIO_LOW, CS_HIGH, DIR]);
}

/// Value from the bytes clocked in after a read command.
fn parse(window: &[u8]) -> Option<u8> {
    let mut it = window.iter();
    while let Some(&b) = it.next() {
        match b {
            EDI_NOT_READY => continue,
            EDI_READY => return it.next().copied(),
            _ => return None,
        }
    }
    None
}

impl<'a> Edi<'a> {
    /// Start talking EDI, check the part and stop its 8051 core.
    pub fn enter(ftdi: &'a mut Ftdi, part: &EnePart) -> Result<Self> {
        ftdi.write(&[0x86, CLK_DIV, 0x00, GPIO_LOW, CS_HIGH, DIR])?;
        std::thread::sleep(Duration::from_millis(5));
        let mut e = Edi { ftdi, pxcfg: 0 };
        let hw = e.read(HW_VERSION).map_err(|_| {
            Error::Chip(
                "The EC does not answer on EDI. Check the wiring and that the board is powered."
                    .into(),
            )
        })?;
        let id = e.read(EDI_ID)?;
        if (hw, id) != (part.hw_version, part.edi_id) {
            return Err(Error::Chip(format!(
                "EC reports hardware version {hw:02X}, EDI ID {id:02X}; expected {:02X}/{:02X} \
                 for {}",
                part.hw_version, part.edi_id, part.name
            )));
        }
        e.pxcfg = e.read(PXCFG)?;
        e.write(PXCFG, e.pxcfg | PXCFG_8051_RESET)?;
        Ok(e)
    }

    fn write(&mut self, addr: u16, value: u8) -> Result<()> {
        let mut cmd = Vec::with_capacity(16);
        push_write(&mut cmd, addr, value);
        self.ftdi.write(&cmd)
    }

    fn read(&mut self, addr: u16) -> Result<u8> {
        for _ in 0..8 {
            let mut cmd = Vec::with_capacity(20);
            push_read(&mut cmd, addr);
            cmd.push(SEND_IMMEDIATE);
            self.ftdi.write(&cmd)?;
            let w = self.ftdi.read_exact(READ_WINDOW)?;
            if let Some(v) = parse(&w) {
                return Ok(v);
            }
        }
        Err(Error::Chip(format!(
            "EDI read of {addr:04X} gets no answer"
        )))
    }

    fn wait_idle(&mut self, max: Duration) -> Result<()> {
        let start = Instant::now();
        while self.read(EFCFG)? & EFCFG_BUSY != 0 {
            if start.elapsed() > max {
                return Err(Error::Timeout("the EC flash stays busy".into()));
            }
            std::thread::sleep(Duration::from_micros(200));
        }
        Ok(())
    }

    fn set_address(cmd: &mut Vec<u8>, addr: u32) {
        push_write(cmd, EFA2, (addr >> 16) as u8);
        push_write(cmd, EFA1, (addr >> 8) as u8);
        push_write(cmd, EFA0, addr as u8);
    }

    fn write_enable(&mut self, on: bool) -> Result<()> {
        let v = self.read(EFCFG)?;
        self.write(EFCFG, if on { v | EFCFG_WE } else { v & !EFCFG_WE })
    }

    /// Read the whole flash.
    pub fn read_flash(&mut self, part: &EnePart, progress: Progress) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(part.size as usize);
        let mut a = 0;
        while a < part.size {
            let n = READ_BATCH.min(part.size - a);
            let mut cmd = Vec::with_capacity(n as usize * 70);
            for i in a..a + n {
                Self::set_address(&mut cmd, i);
                push_write(&mut cmd, EFCMD, FL_READ);
                push_read(&mut cmd, EFDAT);
            }
            cmd.push(SEND_IMMEDIATE);
            self.ftdi.write(&cmd)?;
            let r = self.ftdi.read_exact(n as usize * READ_WINDOW)?;
            for (k, w) in r.chunks(READ_WINDOW).enumerate() {
                match parse(w) {
                    Some(v) => out.push(v),
                    None => out.push(self.read_byte_slow(a + k as u32)?),
                }
            }
            a += n;
            if !progress(a as u64, part.size as u64) {
                return Err(Error::Cancelled);
            }
        }
        Ok(out)
    }

    fn read_byte_slow(&mut self, addr: u32) -> Result<u8> {
        let mut cmd = Vec::new();
        Self::set_address(&mut cmd, addr);
        push_write(&mut cmd, EFCMD, FL_READ);
        self.ftdi.write(&cmd)?;
        self.wait_idle(Duration::from_millis(10))?;
        self.read(EFDAT)
    }

    /// Erase the sector holding `addr`.
    pub fn erase_sector(&mut self, addr: u32) -> Result<()> {
        self.write_enable(true)?;
        let mut cmd = Vec::new();
        Self::set_address(&mut cmd, addr);
        push_write(&mut cmd, EFCMD, FL_ERASE);
        self.ftdi.write(&cmd)?;
        let r = self.wait_idle(Duration::from_millis(200));
        self.write_enable(false)?;
        r
    }

    /// Program one page (must be erased).
    pub fn program_page(&mut self, addr: u32, data: &[u8]) -> Result<()> {
        self.write_enable(true)?;
        let mut cmd = Vec::new();
        push_write(&mut cmd, EFCMD, FL_CLEAR_LATCHES);
        for (i, &b) in data.iter().enumerate() {
            Self::set_address(&mut cmd, addr + i as u32);
            push_write(&mut cmd, EFDAT, b);
            push_write(&mut cmd, EFCMD, FL_LATCH);
        }
        Self::set_address(&mut cmd, addr);
        push_write(&mut cmd, EFCMD, FL_PROGRAM);
        self.ftdi.write(&cmd)?;
        let r = self.wait_idle(Duration::from_millis(50));
        self.write_enable(false)?;
        r
    }

    /// Erase every sector that is not blank.
    pub fn erase(&mut self, part: &EnePart, progress: Progress) -> Result<()> {
        let cur = self.read_flash(part, &mut |_, _| true)?;
        for (i, s) in cur.chunks(part.sector as usize).enumerate() {
            if s.iter().any(|&b| b != 0xFF) {
                self.erase_sector(i as u32 * part.sector)?;
            }
            if !progress(((i + 1) as u32 * part.sector) as u64, part.size as u64) {
                return Err(Error::Cancelled);
            }
        }
        Ok(())
    }

    /// Write `data` from address 0: sectors that change are erased, pages
    /// that are not all `FF` are programmed.
    pub fn write_flash(&mut self, part: &EnePart, data: &[u8], progress: Progress) -> Result<()> {
        let cur = self.read_flash(part, &mut |_, _| true)?;
        let sector = part.sector as usize;
        for (i, want) in data.chunks(sector).enumerate() {
            let base = i * sector;
            let have = &cur[base..base + want.len()];
            if want == have {
                continue;
            }
            if have.iter().any(|&b| b != 0xFF) {
                self.erase_sector(base as u32)?;
            }
            for (j, page) in want.chunks(part.page as usize).enumerate() {
                if page.iter().any(|&b| b != 0xFF) {
                    self.program_page((base + j * part.page as usize) as u32, page)?;
                }
            }
            if !progress((base + want.len()) as u64, data.len() as u64) {
                return Err(Error::Cancelled);
            }
        }
        progress(data.len() as u64, data.len() as u64);
        Ok(())
    }

    /// Let the 8051 run again and leave EDI mode.
    pub fn leave(mut self) -> Result<()> {
        let restore = self.write(PXCFG, self.pxcfg & !PXCFG_8051_RESET);
        let mut cmd = vec![GPIO_LOW, 0x00, DIR, OUT, 0x00, 0x00, EDI_DISABLE];
        cmd.extend_from_slice(&[GPIO_LOW, CS_HIGH, DIR]);
        self.ftdi.write(&cmd)?;
        restore
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ready_marker() {
        assert_eq!(parse(&[0x5F, 0x5F, 0x50, 0xAB]), Some(0xAB));
        assert_eq!(parse(&[0x50, 0x50, 0, 0]), Some(0x50));
        assert_eq!(parse(&[0x5F, 0x5F, 0x5F, 0x50]), None);
        assert_eq!(parse(&[0xFF, 0xFF, 0xFF, 0xFF]), None);
        assert!(find("KB9012@LQFP128").is_some());
    }
}
