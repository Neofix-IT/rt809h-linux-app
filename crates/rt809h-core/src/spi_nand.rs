//! SPI NAND flash (W25N, MX35, GD5F, ...): the common command set with a
//! page cache, row/column addressing and a status/feature register.
//! Data is read and written raw, spare area included.

use std::time::{Duration, Instant};

use crate::spi::{Frame, Progress, SpiBus};
use crate::{Error, Result};

mod op {
    pub const RESET: u8 = 0xFF;
    pub const GET_FEATURE: u8 = 0x0F;
    pub const SET_FEATURE: u8 = 0x1F;
    pub const READ_ID: u8 = 0x9F;
    pub const PAGE_READ: u8 = 0x13;
    pub const READ_CACHE: u8 = 0x03;
    pub const WREN: u8 = 0x06;
    pub const PROGRAM_LOAD: u8 = 0x02;
    pub const PROGRAM_EXEC: u8 = 0x10;
    pub const BLOCK_ERASE: u8 = 0xD8;
}

const REG_PROTECT: u8 = 0xA0;
const REG_CONFIG: u8 = 0xB0;
const REG_STATUS: u8 = 0xC0;
const ST_BUSY: u8 = 0x01;
const ST_E_FAIL: u8 = 0x04;
const ST_P_FAIL: u8 = 0x08;
const CFG_ECC: u8 = 0x10;
/// pages per erase block on every part in the list
const PAGES_PER_BLOCK: u32 = 64;
/// longest page-read time (with on-die ECC) on the listed parts
const T_READ_US: u32 = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NandGeometry {
    /// bytes per page including the spare area
    pub page: u32,
    pub pages: u32,
    /// keep the on-die ECC enabled (parts listed as "(ECC_EN)")
    pub ecc: bool,
}

impl NandGeometry {
    /// Geometry from the raw capacity (spare included) and an optional
    /// page size from the part record.
    pub fn from_raw(raw: u32, page: u32, ecc: bool) -> Option<Self> {
        let main = 1u32 << (31 - raw.leading_zeros());
        let spare_per_2k = (raw - main) / (main / 2048);
        let page = match page {
            0 => 2048 + spare_per_2k,
            p => p,
        };
        (main >= 1 << 20 && spare_per_2k > 0 && raw.is_multiple_of(page)).then(|| NandGeometry {
            page,
            pages: raw / page,
            ecc,
        })
    }

    pub fn size(&self) -> u32 {
        self.page * self.pages
    }
}

pub fn read_id(bus: &mut SpiBus) -> Result<[u8; 2]> {
    let r = bus.transfer(&[op::READ_ID, 0x00], 2)?;
    Ok([r[0], r[1]])
}

pub struct Nand<'a> {
    bus: SpiBus<'a>,
    geo: NandGeometry,
}

fn row(p: u32) -> [u8; 3] {
    [(p >> 16) as u8, (p >> 8) as u8, p as u8]
}

impl<'a> Nand<'a> {
    pub fn new(mut bus: SpiBus<'a>, geo: NandGeometry) -> Result<Self> {
        bus.transfer(&[op::RESET], 0)?;
        std::thread::sleep(Duration::from_millis(5));
        let mut n = Nand { bus, geo };
        n.wait(Duration::from_millis(100))?;
        // clear block protection, set the ECC mode
        n.set_feature(REG_PROTECT, 0x00)?;
        let cfg = n.get_feature(REG_CONFIG)?;
        let want = if geo.ecc {
            cfg | CFG_ECC
        } else {
            cfg & !CFG_ECC
        };
        if want != cfg {
            n.set_feature(REG_CONFIG, want)?;
        }
        Ok(n)
    }

    fn get_feature(&mut self, reg: u8) -> Result<u8> {
        Ok(self.bus.transfer(&[op::GET_FEATURE, reg], 1)?[0])
    }

    fn set_feature(&mut self, reg: u8, v: u8) -> Result<()> {
        self.bus.transfer(&[op::SET_FEATURE, reg, v], 0).map(|_| ())
    }

    fn wait(&mut self, timeout: Duration) -> Result<u8> {
        let t = Instant::now();
        loop {
            let s = self.get_feature(REG_STATUS)?;
            if s & ST_BUSY == 0 {
                return Ok(s);
            }
            if t.elapsed() > timeout {
                return Err(Error::Timeout("SPI NAND stays busy".into()));
            }
        }
    }

    /// Read `count` pages starting at `first`, several pages per USB transfer.
    fn read_pages(&mut self, first: u32, count: u32) -> Result<Vec<u8>> {
        let page = self.geo.page as usize;
        let cmds: Vec<([u8; 4], [u8; 4])> = (first..first + count)
            .map(|p| {
                let r = row(p);
                ([op::PAGE_READ, r[0], r[1], r[2]], [op::READ_CACHE, 0, 0, 0])
            })
            .collect();
        let status = [op::GET_FEATURE, REG_STATUS];
        let mut frames = Vec::new();
        for (pr, rc) in &cmds {
            frames.push(Frame {
                out: pr,
                in_len: 0,
                delay_us: T_READ_US,
            });
            frames.push(Frame {
                out: &status,
                in_len: 1,
                delay_us: 0,
            });
            frames.push(Frame {
                out: rc,
                in_len: page,
                delay_us: 0,
            });
        }
        let raw = self.bus.batch(&frames)?;
        let mut out = Vec::with_capacity(page * count as usize);
        for (i, chunk) in raw.chunks(page + 1).enumerate() {
            if chunk[0] & ST_BUSY != 0 {
                // slow part: read this page with polling
                let p = first + i as u32;
                let r = row(p);
                self.bus.transfer(&[op::PAGE_READ, r[0], r[1], r[2]], 0)?;
                self.wait(Duration::from_millis(10))?;
                out.extend(self.bus.transfer(&[op::READ_CACHE, 0, 0, 0], page)?);
            } else {
                out.extend_from_slice(&chunk[1..]);
            }
        }
        Ok(out)
    }

    pub fn read(&mut self, progress: Progress) -> Result<Vec<u8>> {
        const BATCH: u32 = 16;
        let total = self.geo.size() as u64;
        let mut out = Vec::with_capacity(total as usize);
        let mut p = 0;
        while p < self.geo.pages {
            let n = BATCH.min(self.geo.pages - p);
            out.extend(self.read_pages(p, n)?);
            p += n;
            if !progress(out.len() as u64, total) {
                return Err(Error::Cancelled);
            }
        }
        Ok(out)
    }

    /// Erase every block; returns the numbers of blocks that failed (bad).
    pub fn erase(&mut self, progress: Progress) -> Result<Vec<u32>> {
        let blocks = self.geo.pages / PAGES_PER_BLOCK;
        let mut bad = Vec::new();
        for b in 0..blocks {
            let r = row(b * PAGES_PER_BLOCK);
            self.bus.batch(&[
                Frame {
                    out: &[op::WREN],
                    in_len: 0,
                    delay_us: 0,
                },
                Frame {
                    out: &[op::BLOCK_ERASE, r[0], r[1], r[2]],
                    in_len: 0,
                    delay_us: 0,
                },
            ])?;
            if self.wait(Duration::from_millis(50))? & ST_E_FAIL != 0 {
                bad.push(b);
            }
            if !progress((b + 1) as u64, blocks as u64) {
                return Err(Error::Cancelled);
            }
        }
        Ok(bad)
    }

    /// Program pages from the start of the chip; all-FF pages are skipped.
    /// Returns the pages that failed to program.
    pub fn program(&mut self, data: &[u8], progress: Progress) -> Result<Vec<u32>> {
        let page = self.geo.page as usize;
        let mut failed = Vec::new();
        let total = data.len() as u64;
        for (i, chunk) in data.chunks(page).enumerate() {
            if chunk.iter().any(|&b| b != 0xFF) {
                let r = row(i as u32);
                let mut load = vec![op::PROGRAM_LOAD, 0, 0];
                load.extend_from_slice(chunk);
                self.bus.batch(&[
                    Frame {
                        out: &[op::WREN],
                        in_len: 0,
                        delay_us: 0,
                    },
                    Frame {
                        out: &load,
                        in_len: 0,
                        delay_us: 0,
                    },
                    Frame {
                        out: &[op::PROGRAM_EXEC, r[0], r[1], r[2]],
                        in_len: 0,
                        delay_us: 0,
                    },
                ])?;
                if self.wait(Duration::from_millis(20))? & ST_P_FAIL != 0 {
                    failed.push(i as u32);
                }
            }
            if !progress(((i + 1) * page).min(data.len()) as u64, total) {
                return Err(Error::Cancelled);
            }
        }
        Ok(failed)
    }
}

/// 1.8 V SPI NAND product lines, recognised by part number.
pub fn low_voltage(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    let base = n.split(['(', '@']).next().unwrap_or("");
    [
        ("W25N", "GW"),
        ("W25N", "JW"),
        ("MX35UF", ""),
        ("TC58CYG", ""),
        ("DS35M", ""),
        ("F50D", ""),
        ("GD5F", "Q4R"),
        ("GD5F", "Q5R"),
        ("GD5F", "Q6R"),
        ("GD5F", "RE"),
        ("XT26Q", ""),
        ("F35SQB", ""),
    ]
    .iter()
    .any(|(p, s)| base.starts_with(p) && base.contains(s))
        || (base.starts_with("MT29F") && base.contains("ABB"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry() {
        let g = NandGeometry::from_raw(138_412_032, 0, false).unwrap();
        assert_eq!((g.page, g.pages), (2112, 65536));
        let g = NandGeometry::from_raw(285_212_672, 0, false).unwrap();
        assert_eq!((g.page, g.pages), (2176, 131072));
        let g = NandGeometry::from_raw(570_425_344, 4352, false).unwrap();
        assert_eq!((g.page, g.pages), (4352, 131072));
        assert!(NandGeometry::from_raw(16 << 20, 0, false).is_none());
    }

    #[test]
    fn voltage() {
        assert!(low_voltage("W25N512GW(OTP)(ECC_EN)(X4)@WSON8"));
        assert!(!low_voltage("W25N01GV(ECC_DIS)@WSON8"));
        assert!(low_voltage("DS35M1GB(ECC_DIS)@WSON8"));
        assert!(!low_voltage("DS35Q1GA(ECC_EN)@SOP16"));
    }
}
