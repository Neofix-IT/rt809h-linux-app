//! 25xx / 95xx SPI EEPROMs (and SPI MRAM/FRAM with the same command set).

use std::time::{Duration, Instant};

use crate::spi::{op, Frame, Progress, SpiBus};
use crate::{Error, Result};

pub struct SpiEeprom<'a> {
    bus: SpiBus<'a>,
    size: u32,
}

/// Largest write that never crosses a page on any part of this capacity.
fn page_size(size: u32) -> usize {
    match size {
        0..=2048 => 16,
        2049..=8192 => 32,
        8193..=32768 => 64,
        32769..=65536 => 128,
        _ => 256,
    }
}

impl<'a> SpiEeprom<'a> {
    pub fn new(mut bus: SpiBus<'a>, size: u32) -> Result<Self> {
        // a missing chip reads as all ones
        if bus.transfer(&[op::RDSR], 1)?[0] == 0xFF {
            return Err(Error::Chip("No SPI EEPROM answers".into()));
        }
        // clear the block protection bits
        bus.batch(&[
            Frame {
                out: &[op::WREN],
                in_len: 0,
                delay_us: 0,
            },
            Frame {
                out: &[op::WRSR, 0x00],
                in_len: 0,
                delay_us: 0,
            },
        ])?;
        let mut e = SpiEeprom { bus, size };
        e.wait()?;
        Ok(e)
    }

    /// Command byte plus address for `addr`.
    fn header(&self, cmd: u8, addr: u32) -> Vec<u8> {
        match self.size {
            0..=256 => vec![cmd, addr as u8],
            // 512-byte parts carry A8 in bit 3 of the opcode
            257..=512 => vec![cmd | (((addr >> 8) as u8 & 1) << 3), addr as u8],
            513..=65536 => vec![cmd, (addr >> 8) as u8, addr as u8],
            _ => vec![cmd, (addr >> 16) as u8, (addr >> 8) as u8, addr as u8],
        }
    }

    fn wait(&mut self) -> Result<()> {
        let t = Instant::now();
        while self.bus.transfer(&[op::RDSR], 1)?[0] & 1 != 0 {
            if t.elapsed() > Duration::from_millis(100) {
                return Err(Error::Timeout("SPI EEPROM stays busy".into()));
            }
        }
        Ok(())
    }

    pub fn read(&mut self, progress: Progress) -> Result<Vec<u8>> {
        let size = self.size as usize;
        let h = self.header(op::READ, 0);
        if self.size == 512 {
            // the opcode selects the half; read both
            let mut out = self.bus.transfer(&h, 256)?;
            let h2 = self.header(op::READ, 256);
            out.extend(self.bus.transfer(&h2, 256)?);
            progress(512, 512);
            return Ok(out);
        }
        self.bus
            .read_stream(&h, size, |n| progress(n as u64, size as u64))
    }

    pub fn write(&mut self, data: &[u8], progress: Progress) -> Result<()> {
        let page = page_size(self.size);
        let mut done = 0;
        for (i, chunk) in data.chunks(page).enumerate() {
            let mut w = self.header(op::PP, (i * page) as u32);
            w.extend_from_slice(chunk);
            self.bus.batch(&[
                Frame {
                    out: &[op::WREN],
                    in_len: 0,
                    delay_us: 0,
                },
                Frame {
                    out: &w,
                    in_len: 0,
                    delay_us: 0,
                },
            ])?;
            self.wait()?;
            done += chunk.len();
            if !progress(done as u64, data.len() as u64) {
                return Err(Error::Cancelled);
            }
        }
        Ok(())
    }
}

/// Capacity from a 25xx/95xx part number ("25LC640" -> 8 KiB, "M95M01" -> 128 KiB).
pub fn size_from_name(name: &str) -> Option<u32> {
    let base = name.split(['@', '(']).next()?.to_ascii_uppercase();
    let b = base.as_bytes();
    (0..b.len().saturating_sub(1))
        .filter(|&i| matches!(&b[i..i + 2], b"25" | b"95"))
        .find_map(|i| crate::i2c::series_size(&base[i + 2..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(size_from_name("25AA080"), Some(1024));
        assert_eq!(size_from_name("25LC640A@SOIC8"), Some(8192));
        assert_eq!(size_from_name("M95M01"), Some(131072));
        assert_eq!(size_from_name("25AA1024(ISP)"), Some(131072));
        assert_eq!(size_from_name("25AA040A"), Some(512));
        assert_eq!(page_size(512), 16);
    }
}
