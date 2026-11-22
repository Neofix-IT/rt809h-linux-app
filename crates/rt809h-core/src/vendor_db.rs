//! Full part list (names, makers, capacities, JEDEC IDs) for every chip the
//! RT809H handles, with the chip family each part belongs to. Only chip
//! specifications are stored here; see `data/vendor_chips.tsv`.

use std::sync::OnceLock;

use crate::chips::{self, Chip};
use crate::protocol::Voltage;
use crate::{avr, ene, i2c, image, spi_eeprom, spi_nand};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Family {
    Spi,
    I2c,
    Microwire,
    ParallelNor,
    Eprom,
    Eeprom,
    Nand,
    Emmc,
    Mcu,
    Pld,
    OneWire,
}

impl Family {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "spi" => Family::Spi,
            "i2c" => Family::I2c,
            "mw" => Family::Microwire,
            "nor" => Family::ParallelNor,
            "eprom" => Family::Eprom,
            "eeprom" => Family::Eeprom,
            "nand" => Family::Nand,
            "emmc" => Family::Emmc,
            "mcu" => Family::Mcu,
            "pld" => Family::Pld,
            "1wire" => Family::OneWire,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Family::Spi => "SPI flash",
            Family::I2c => "I2C EEPROM",
            Family::Microwire => "Microwire EEPROM",
            Family::ParallelNor => "Parallel NOR flash",
            Family::Eprom => "EPROM",
            Family::Eeprom => "Parallel EEPROM",
            Family::Nand => "NAND flash",
            Family::Emmc => "eMMC",
            Family::Mcu => "Microcontroller",
            Family::Pld => "PLD",
            Family::OneWire => "1-Wire EEPROM",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct VendorChip {
    pub family: Family,
    pub maker: &'static str,
    pub name: &'static str,
    pub size: u32,
    pub id: u32,
    /// programming algorithm reference (logic image name for families that
    /// need one)
    pub algo: &'static str,
    /// erase / program timeouts from the part record (image families)
    pub t_erase: u32,
    pub t_program: u32,
    /// write buffer / page sizes from the part record (image families)
    pub buffer: u32,
    pub page: u32,
}

/// What the native driver does with a part.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Driver {
    /// 25-series SPI NOR flash with the standard command set.
    Spi(Chip),
    /// 24xx I2C EEPROM of this many bytes.
    I2c(u32),
    /// Runs on a logic image inside the programmer.
    Image(&'static str, image::Setup),
    /// SPI NAND flash (raw pages, spare area included).
    SpiNand(SpiNandPart),
    /// 25xx/95xx SPI EEPROM of this many bytes.
    SpiEeprom(u32),
    /// AVR microcontroller on the ISP header.
    Avr(&'static avr::AvrPart, avr::Memory),
    /// ENE embedded controller, in circuit over EDI.
    Ene(&'static ene::EnePart),
}

impl Driver {
    /// Bytes read from / written to the part.
    pub fn size(&self) -> u32 {
        match self {
            Driver::Spi(c) => c.size,
            Driver::I2c(n) | Driver::SpiEeprom(n) => *n,
            Driver::Image(_, s) => s.size,
            Driver::SpiNand(n) => n.geometry.size(),
            Driver::Avr(p, avr::Memory::Flash) => p.flash,
            Driver::Avr(p, avr::Memory::Eeprom) => p.eeprom as u32,
            Driver::Ene(p) => p.size,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpiNandPart {
    pub geometry: spi_nand::NandGeometry,
    pub voltage: Voltage,
    /// maker and device ID bytes, if the part has a known ID
    pub id: Option<[u8; 2]>,
}

impl VendorChip {
    pub fn driver(&self) -> Option<Driver> {
        if self.family == Family::Mcu && self.algo == "ALGO_KB9012" {
            if let Some(p) = ene::find(self.name) {
                return Some(Driver::Ene(p));
            }
        }
        if self.family == Family::Mcu {
            if let Some(p) = avr::find(self.name) {
                return Some(Driver::Avr(p, avr::Memory::Flash));
            }
        }
        match self.family {
            Family::Spi
                if ["25_", "ALGO_25_", "ALGO_SPI_1_OTP", "S25FL_"]
                    .iter()
                    .any(|p| self.algo.starts_with(p))
                    && self.id != 0
                    && self.size > 0 =>
            {
                Some(Driver::Spi(self.as_spi_chip()))
            }
            Family::Spi if self.algo == "ALGO_SPINAND_1" => {
                let ecc = self.name.contains("(ECC_EN)");
                let geometry = spi_nand::NandGeometry::from_raw(self.size, self.buffer, ecc)?;
                let id = [(self.id >> 8) as u8, self.id as u8];
                Some(Driver::SpiNand(SpiNandPart {
                    geometry,
                    voltage: if spi_nand::low_voltage(self.name) {
                        Voltage::V1_8
                    } else {
                        Voltage::V3_3
                    },
                    id: (id != [0, 0]).then_some(id),
                }))
            }
            Family::Spi if self.algo.starts_with("SPI_95") => spi_eeprom::size_from_name(self.name)
                .or((self.size > 0).then_some(self.size))
                .map(Driver::SpiEeprom),
            Family::I2c if self.algo.starts_with("24_") => {
                i2c::size_from_name(self.name).map(Driver::I2c)
            }
            Family::Spi | Family::I2c | Family::OneWire => None,
            _ => {
                let size = match self.family {
                    Family::Microwire => image::microwire::size_from_name(self.name)?,
                    _ if self.size > 0 => self.size,
                    _ => return None,
                };
                let mut s = image::Setup::new(
                    image::profile(self.algo)?,
                    size,
                    self.t_erase,
                    self.t_program,
                );
                if self.buffer > 0 {
                    s.buffer = self.buffer;
                }
                if self.page > 0 {
                    s.page = self.page as u16;
                }
                Some(Driver::Image(self.algo, s))
            }
        }
    }

    pub fn supported(&self) -> bool {
        self.driver().is_some()
    }

    fn as_spi_chip(&self) -> Chip {
        let known = chips::CHIPS.iter().find(|c| c.id == self.id);
        Chip {
            vendor: if self.maker.is_empty() {
                known
                    .map(|c| c.vendor)
                    .unwrap_or_else(|| chips::manufacturer_name((self.id >> 16) as u8))
            } else {
                self.maker
            },
            name: self.name,
            id: self.id,
            size: self.size,
            voltage: known
                .map(|c| c.voltage)
                .unwrap_or_else(|| guess_voltage(self.id)),
            family: known.map(|c| c.family).unwrap_or(chips::Family::Standard),
        }
    }
}

/// Supply voltage for an SPI flash that is not in the curated table:
/// the 1.8 V product lines of the large vendors use their own ID ranges.
fn guess_voltage(id: u32) -> Voltage {
    let (mfr, kind) = ((id >> 16) as u8, (id >> 8) as u8);
    let low = matches!(
        (mfr, kind),
        (0xEF, 0x60)
            | (0xEF, 0x80)
            | (0xC2, 0x25)
            | (0xC2, 0x28)
            | (0xC8, 0x60)
            | (0xC8, 0x63)
            | (0x1C, 0x38)
            | (0x20, 0xBB)
            | (0x9D, 0x70)
    );
    if low {
        Voltage::V1_8
    } else {
        Voltage::V3_3
    }
}

static DATA: &str = include_str!("../data/vendor_chips.tsv");

pub fn all() -> &'static [VendorChip] {
    static ALL: OnceLock<Vec<VendorChip>> = OnceLock::new();
    ALL.get_or_init(|| {
        DATA.lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
            .filter_map(|l| {
                let mut f = l.split('\t');
                Some(VendorChip {
                    family: Family::parse(f.next()?)?,
                    maker: f.next()?,
                    name: f.next()?,
                    size: f.next()?.parse().ok()?,
                    id: u32::from_str_radix(f.next()?, 16).ok()?,
                    algo: f.next().unwrap_or(""),
                    t_erase: f.next().and_then(|v| v.parse().ok()).unwrap_or(0),
                    t_program: f.next().and_then(|v| v.parse().ok()).unwrap_or(0),
                    buffer: f.next().and_then(|v| v.parse().ok()).unwrap_or(0),
                    page: f.next().and_then(|v| v.parse().ok()).unwrap_or(0),
                })
            })
            .chain(avr::all_parts().map(|p| VendorChip {
                family: Family::Mcu,
                maker: "Microchip (Atmel)",
                name: p.name,
                size: p.flash,
                id: u32::from_be_bytes([0, p.signature[0], p.signature[1], p.signature[2]]),
                algo: match p.bus {
                    avr::Bus::Isp => "AVR_ISP",
                    avr::Bus::Updi(_) => "AVR_UPDI",
                },
                t_erase: 0,
                t_program: 0,
                buffer: p.page as u32,
                page: 0,
            }))
            .collect()
    })
}

/// Case-insensitive search on maker and part name.
pub fn search(q: &str) -> Vec<&'static VendorChip> {
    let q = q.to_ascii_uppercase();
    all()
        .iter()
        .filter(|c| {
            q.is_empty()
                || c.name.to_ascii_uppercase().contains(&q)
                || c.maker.to_ascii_uppercase().contains(&q)
        })
        .collect()
}

/// Parts the native driver handles (computed once).
pub fn supported() -> &'static [&'static VendorChip] {
    static S: OnceLock<Vec<&'static VendorChip>> = OnceLock::new();
    S.get_or_init(|| all().iter().filter(|c| c.supported()).collect())
}

/// Like [`search`], limited to supported parts.
pub fn search_supported(q: &str) -> Vec<&'static VendorChip> {
    let q = q.to_ascii_uppercase();
    supported()
        .iter()
        .copied()
        .filter(|c| {
            q.is_empty()
                || c.name.to_ascii_uppercase().contains(&q)
                || c.maker.to_ascii_uppercase().contains(&q)
        })
        .collect()
}

/// Exact part name (case-insensitive), preferring supported entries.
pub fn find(name: &str) -> Option<&'static VendorChip> {
    let mut hits = all().iter().filter(|c| c.name.eq_ignore_ascii_case(name));
    let first = hits.next()?;
    Some(
        std::iter::once(first)
            .chain(hits)
            .find(|c| c.supported())
            .unwrap_or(first),
    )
}

/// (supported, total) part counts per family.
pub fn coverage() -> Vec<(Family, usize, usize)> {
    let mut v: Vec<(Family, usize, usize)> = Vec::new();
    for c in all() {
        let i = match v.iter().position(|e| e.0 == c.family) {
            Some(i) => i,
            None => {
                v.push((c.family, 0, 0));
                v.len() - 1
            }
        };
        v[i].2 += 1;
        if c.supported() {
            v[i].1 += 1;
        }
    }
    v.sort_by_key(|e| std::cmp::Reverse(e.2));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_and_classifies() {
        assert!(all().len() > 20000);
        let c = find("W25Q128BV@WSON8").unwrap();
        assert_eq!(c.id, 0xEF4018);
        assert!(matches!(c.driver(), Some(Driver::Spi(_))));
        let e = find("AT24C02D").unwrap();
        assert_eq!(e.driver(), Some(Driver::I2c(256)));
        let n = find("SST39SF040").unwrap();
        assert_eq!(
            n.driver(),
            Some(Driver::Image(
                "ALGO_NOR_8_1",
                image::nor::setup(0x80000, 50000, 500)
            ))
        );
        let a = find("ATmega328P").unwrap();
        assert!(matches!(a.driver(), Some(Driver::Avr(p, _)) if p.flash == 32768));
        let m = find("93C46").unwrap();
        assert_eq!(
            m.driver(),
            Some(Driver::Image("ALGO_93_3_16", image::microwire::setup(128)))
        );
    }
}
