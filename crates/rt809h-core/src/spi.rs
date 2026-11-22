//! SPI master on the FT2232H MPSSE engine and 25-series NOR flash commands.

use std::time::{Duration, Instant};

use crate::ftdi::{BitMode, Ftdi};
use crate::{Error, Result};

// ADBUS: bit0 SCK, bit1 MOSI, bit2 MISO, bit3 CS#
const DIR: u8 = 0x0B;
const CS_HIGH: [u8; 3] = [0x80, 0x08, DIR];
const CS_LOW: [u8; 3] = [0x80, 0x00, DIR];
const MAX_XFER: usize = 65536;

pub struct SpiBus<'a> {
    ftdi: &'a mut Ftdi,
    hz: u32,
}

/// One chip-select cycle of a batched transfer.
pub struct Frame<'b> {
    pub out: &'b [u8],
    pub in_len: usize,
    /// idle clocks after CS# goes high, in microseconds
    pub delay_us: u32,
}

impl<'a> SpiBus<'a> {
    /// Put the channel into MPSSE mode with the given SCK frequency.
    pub fn init(ftdi: &'a mut Ftdi, hz: u32) -> Result<Self> {
        ftdi.set_bitmode(0xFF, BitMode::Reset)?;
        ftdi.set_bitmode(0xFF, BitMode::Mpsse)?;
        let mut bus = Self { ftdi, hz };
        bus.set_speed(hz)?;
        Ok(bus)
    }

    pub fn set_speed(&mut self, hz: u32) -> Result<()> {
        // 60 MHz base clock: f = 60 MHz / (2 * (div + 1))
        let div = (60_000_000 / (2 * hz.max(1))).saturating_sub(1).min(0xFFFF) as u16;
        self.hz = 30_000_000 / (div as u32 + 1);
        let mut c = vec![0x8A, 0x97, 0x8D, 0x86, div as u8, (div >> 8) as u8];
        c.extend_from_slice(&CS_HIGH);
        self.ftdi.write(&c)
    }

    fn push_out(c: &mut Vec<u8>, data: &[u8]) {
        for chunk in data.chunks(MAX_XFER) {
            let n = chunk.len() - 1;
            c.extend_from_slice(&[0x11, n as u8, (n >> 8) as u8]);
            c.extend_from_slice(chunk);
        }
    }

    fn push_in(c: &mut Vec<u8>, len: usize) {
        let mut left = len;
        while left > 0 {
            let n = left.min(MAX_XFER);
            c.extend_from_slice(&[0x20, (n - 1) as u8, ((n - 1) >> 8) as u8]);
            left -= n;
        }
    }

    /// One chip-select cycle: send `out`, then clock in `in_len` bytes.
    pub fn transfer(&mut self, out: &[u8], in_len: usize) -> Result<Vec<u8>> {
        let mut c = Vec::with_capacity(out.len() + 16);
        c.extend_from_slice(&CS_LOW);
        Self::push_out(&mut c, out);
        Self::push_in(&mut c, in_len);
        c.extend_from_slice(&CS_HIGH);
        if in_len > 0 {
            c.push(0x87);
        }
        self.ftdi.write(&c)?;
        if in_len > 0 {
            self.ftdi.read_exact(in_len)
        } else {
            Ok(Vec::new())
        }
    }

    /// Several write-only chip-select cycles in one USB transfer.
    pub fn write_batch(&mut self, frames: &[&[u8]]) -> Result<()> {
        let mut c = Vec::new();
        for f in frames {
            c.extend_from_slice(&CS_LOW);
            Self::push_out(&mut c, f);
            c.extend_from_slice(&CS_HIGH);
        }
        self.ftdi.write(&c)
    }

    /// Several chip-select cycles in one USB round trip; returns all bytes
    /// clocked in, in order.
    pub fn batch(&mut self, frames: &[Frame]) -> Result<Vec<u8>> {
        let mut c = Vec::new();
        let mut total = 0;
        for f in frames {
            c.extend_from_slice(&CS_LOW);
            Self::push_out(&mut c, f.out);
            Self::push_in(&mut c, f.in_len);
            c.extend_from_slice(&CS_HIGH);
            total += f.in_len;
            // clock without data to wait (8 clocks per count)
            let mut bytes = (f.delay_us as u64 * self.hz as u64 / 8_000_000) as usize;
            while bytes > 0 {
                let n = bytes.min(65536);
                c.extend_from_slice(&[0x8F, (n - 1) as u8, ((n - 1) >> 8) as u8]);
                bytes -= n;
            }
        }
        if total > 0 {
            c.push(0x87);
        }
        self.ftdi.write(&c)?;
        if total > 0 {
            self.ftdi.read_exact(total)
        } else {
            Ok(Vec::new())
        }
    }

    /// Long read with the command/address sent once and data streamed.
    pub fn read_stream(
        &mut self,
        header: &[u8],
        len: usize,
        mut progress: impl FnMut(usize) -> bool,
    ) -> Result<Vec<u8>> {
        const CHUNK: usize = 256 * 1024;
        let mut out = Vec::with_capacity(len);
        let mut c = Vec::new();
        c.extend_from_slice(&CS_LOW);
        Self::push_out(&mut c, header);
        self.ftdi.write(&c)?;
        while out.len() < len {
            let n = (len - out.len()).min(CHUNK);
            let mut c = Vec::new();
            Self::push_in(&mut c, n);
            c.push(0x87);
            self.ftdi.write(&c)?;
            out.extend(self.ftdi.read_exact(n)?);
            if !progress(out.len()) {
                self.ftdi.write(&CS_HIGH)?;
                return Err(Error::Cancelled);
            }
        }
        self.ftdi.write(&CS_HIGH)?;
        Ok(out)
    }
}

pub mod op {
    pub const WRSR: u8 = 0x01;
    pub const PP: u8 = 0x02;
    pub const READ: u8 = 0x03;
    pub const WRDI: u8 = 0x04;
    pub const RDSR: u8 = 0x05;
    pub const WREN: u8 = 0x06;
    pub const FAST_READ: u8 = 0x0B;
    pub const SE_4K: u8 = 0x20;
    pub const RDSR2: u8 = 0x35;
    pub const EWSR: u8 = 0x50;
    pub const SFDP: u8 = 0x5A;
    pub const ULBPR: u8 = 0x98;
    pub const RDID: u8 = 0x9F;
    pub const RES: u8 = 0xAB;
    pub const AAI_WORD: u8 = 0xAD;
    pub const EN4B: u8 = 0xB7;
    pub const CE: u8 = 0xC7;
    pub const BE_64K: u8 = 0xD8;
    pub const EX4B: u8 = 0xE9;
}

/// JEDEC identification bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JedecId(pub [u8; 3]);

impl JedecId {
    pub fn is_valid(&self) -> bool {
        self.0 != [0, 0, 0] && self.0 != [0xFF, 0xFF, 0xFF] && self.0[0] != 0xFF && self.0[0] != 0
    }

    pub fn as_u32(&self) -> u32 {
        (self.0[0] as u32) << 16 | (self.0[1] as u32) << 8 | self.0[2] as u32
    }
}

impl std::fmt::Display for JedecId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:02X} {:02X} {:02X}", self.0[0], self.0[1], self.0[2])
    }
}

/// Parameters needed to drive a NOR flash.
#[derive(Debug, Clone)]
pub struct FlashGeometry {
    pub size: u32,
    pub page_size: u32,
    /// Smallest erase unit supported with opcode 0x20 (bytes), if any.
    pub sector_size: u32,
    /// SST25-style chips without page program (use AAI word program).
    pub aai: bool,
    /// SST26-style chips that need a global unlock (0x98).
    pub global_unlock: bool,
}

impl FlashGeometry {
    pub fn needs_4byte(&self) -> bool {
        self.size > 16 * 1024 * 1024
    }
}

/// A NOR flash reachable over SPI.
pub struct Flash<'a> {
    pub bus: SpiBus<'a>,
    pub geo: FlashGeometry,
    addr4: bool,
}

/// Progress callback: (bytes done, total). Return false to cancel.
pub type Progress<'p> = &'p mut dyn FnMut(u64, u64) -> bool;

pub fn read_id(bus: &mut SpiBus) -> Result<JedecId> {
    let d = bus.transfer(&[op::RDID], 3)?;
    Ok(JedecId([d[0], d[1], d[2]]))
}

/// Legacy "release from deep power-down / read electronic signature".
pub fn read_signature(bus: &mut SpiBus) -> Result<u8> {
    Ok(bus.transfer(&[op::RES, 0, 0, 0], 1)?[0])
}

/// Read the SFDP header and basic parameter table, if the chip has one.
/// Returns the density in bytes.
pub fn sfdp_size(bus: &mut SpiBus) -> Result<Option<u32>> {
    let hdr = bus.transfer(&[op::SFDP, 0, 0, 0, 0], 16)?;
    if &hdr[0..4] != b"SFDP" {
        return Ok(None);
    }
    // first parameter header at offset 8: id, minor, major, len(dwords), ptr[3]
    let len = hdr[11] as usize * 4;
    let ptr = u32::from_le_bytes([hdr[12], hdr[13], hdr[14], 0]);
    if len < 8 {
        return Ok(None);
    }
    let t = bus.transfer(
        &[op::SFDP, (ptr >> 16) as u8, (ptr >> 8) as u8, ptr as u8, 0],
        8,
    )?;
    let d2 = u32::from_le_bytes([t[4], t[5], t[6], t[7]]);
    let bits: u64 = if d2 & 0x8000_0000 != 0 {
        1u64 << (d2 & 0x7FFF_FFFF)
    } else {
        d2 as u64 + 1
    };
    let bytes = bits / 8;
    if (1024..=(1u64 << 32)).contains(&bytes) {
        Ok(Some(bytes as u32))
    } else {
        Ok(None)
    }
}

impl<'a> Flash<'a> {
    pub fn new(bus: SpiBus<'a>, geo: FlashGeometry) -> Result<Self> {
        let mut f = Self {
            bus,
            geo,
            addr4: false,
        };
        if f.geo.needs_4byte() {
            f.bus.transfer(&[op::EN4B], 0)?;
            f.addr4 = true;
        }
        Ok(f)
    }

    fn addr(&self, opcode: u8, a: u32) -> Vec<u8> {
        if self.addr4 {
            vec![
                opcode,
                (a >> 24) as u8,
                (a >> 16) as u8,
                (a >> 8) as u8,
                a as u8,
            ]
        } else {
            vec![opcode, (a >> 16) as u8, (a >> 8) as u8, a as u8]
        }
    }

    pub fn status(&mut self) -> Result<u8> {
        Ok(self.bus.transfer(&[op::RDSR], 1)?[0])
    }

    fn wait_ready(&mut self, timeout: Duration) -> Result<()> {
        let start = Instant::now();
        loop {
            let s = self.status()?;
            if s & 0x01 == 0 {
                return Ok(());
            }
            if start.elapsed() > timeout {
                return Err(Error::Timeout(format!("chip busy (status {s:02X})")));
            }
        }
    }

    fn wait_ready_with(&mut self, timeout: Duration, progress: Progress) -> Result<()> {
        let start = Instant::now();
        loop {
            let s = self.status()?;
            if s & 0x01 == 0 {
                return Ok(());
            }
            let el = start.elapsed();
            if el > timeout {
                return Err(Error::Timeout(format!("chip busy (status {s:02X})")));
            }
            // erase time is unknown: show a slowly filling bar
            let t = el.as_millis() as u64;
            progress(
                t.min(timeout.as_millis() as u64 - 1),
                timeout.as_millis() as u64,
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Remove block protection so the array can be erased and programmed.
    pub fn unprotect(&mut self) -> Result<()> {
        if self.geo.global_unlock {
            self.bus.write_batch(&[&[op::WREN], &[op::ULBPR]])?;
            return self.wait_ready(Duration::from_secs(1));
        }
        let s = self.status()?;
        if s & 0x9C == 0 {
            return Ok(());
        }
        self.bus
            .write_batch(&[&[op::WREN], &[op::EWSR], &[op::WRSR, 0x00]])?;
        self.wait_ready(Duration::from_secs(1))?;
        let s = self.status()?;
        if s & 0x1C != 0 {
            return Err(Error::Chip(format!(
                "write protection could not be removed (status {s:02X}); check the WP# pin"
            )));
        }
        Ok(())
    }

    pub fn read(&mut self, addr: u32, len: u32, progress: Progress) -> Result<Vec<u8>> {
        let header = self.addr(op::READ, addr);
        let total = len as u64;
        self.bus
            .read_stream(&header, len as usize, |done| progress(done as u64, total))
    }

    pub fn erase_chip(&mut self, progress: Progress) -> Result<()> {
        self.unprotect()?;
        self.bus.write_batch(&[&[op::WREN], &[op::CE]])?;
        // large chips can need several minutes
        let secs = 30 + (self.geo.size as u64 / (1024 * 1024)) * 20;
        self.wait_ready_with(Duration::from_secs(secs), progress)?;
        progress(1, 1);
        Ok(())
    }

    /// Erase the 4 KiB / 64 KiB blocks covering `addr..addr+len`.
    pub fn erase_range(&mut self, addr: u32, len: u32, progress: Progress) -> Result<()> {
        self.unprotect()?;
        let end = addr + len;
        let mut a = addr & !0xFFF;
        let total = (end - a) as u64;
        let start = a;
        while a < end {
            let (opcode, step) = if a.is_multiple_of(0x10000) && end - a >= 0x10000 {
                (op::BE_64K, 0x10000)
            } else {
                (op::SE_4K, 0x1000)
            };
            let cmd = self.addr(opcode, a);
            self.bus.write_batch(&[&[op::WREN], &cmd])?;
            self.wait_ready(Duration::from_secs(10))?;
            a += step;
            if !progress((a - start) as u64, total) {
                return Err(Error::Cancelled);
            }
        }
        Ok(())
    }

    /// Program `data` at `addr`. The area must be erased. Pages that are all
    /// 0xFF are skipped.
    pub fn program(&mut self, addr: u32, data: &[u8], progress: Progress) -> Result<()> {
        if self.geo.aai {
            return self.program_aai(addr, data, progress);
        }
        self.unprotect()?;
        let page = self.geo.page_size.max(1) as usize;
        let total = data.len() as u64;
        let mut off = 0usize;
        while off < data.len() {
            let a = addr as usize + off;
            let n = (page - a % page).min(data.len() - off);
            let chunk = &data[off..off + n];
            if chunk.iter().any(|&b| b != 0xFF) {
                let mut cmd = self.addr(op::PP, a as u32);
                cmd.extend_from_slice(chunk);
                self.bus.write_batch(&[&[op::WREN], &cmd])?;
                self.wait_ready(Duration::from_secs(1))?;
            }
            off += n;
            if !progress(off as u64, total) {
                return Err(Error::Cancelled);
            }
        }
        Ok(())
    }

    /// SST25 auto-address-increment word programming.
    fn program_aai(&mut self, addr: u32, data: &[u8], progress: Progress) -> Result<()> {
        self.unprotect()?;
        let total = data.len() as u64;
        let mut off = 0usize;
        let mut started = false;
        while off < data.len() {
            let w = [data[off], *data.get(off + 1).unwrap_or(&0xFF)];
            if w == [0xFF, 0xFF] {
                if started {
                    self.bus.transfer(&[op::WRDI], 0)?;
                    self.wait_ready(Duration::from_millis(100))?;
                    started = false;
                }
            } else if !started {
                let mut cmd = self.addr(op::AAI_WORD, addr + off as u32);
                cmd.extend_from_slice(&w);
                self.bus.write_batch(&[&[op::WREN], &cmd])?;
                self.wait_ready(Duration::from_millis(100))?;
                started = true;
            } else {
                self.bus.transfer(&[op::AAI_WORD, w[0], w[1]], 0)?;
                self.wait_ready(Duration::from_millis(100))?;
            }
            off += 2;
            if off.is_multiple_of(4096) && !progress(off as u64, total) {
                if started {
                    self.bus.transfer(&[op::WRDI], 0)?;
                }
                return Err(Error::Cancelled);
            }
        }
        if started {
            self.bus.transfer(&[op::WRDI], 0)?;
            self.wait_ready(Duration::from_millis(100))?;
        }
        progress(total, total);
        Ok(())
    }

    pub fn finish(&mut self) -> Result<()> {
        if self.addr4 {
            self.bus.transfer(&[op::EX4B], 0)?;
            self.addr4 = false;
        }
        Ok(())
    }
}
