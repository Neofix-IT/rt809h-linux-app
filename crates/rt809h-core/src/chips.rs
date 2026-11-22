//! Database of 25-series SPI NOR flash chips.
//!
//! Compiled from the manufacturers' public datasheets: JEDEC ID, capacity
//! and supply range. Chips that are not listed are still handled when they
//! report a sane JEDEC ID (capacity from SFDP or the ID's capacity byte).

use crate::protocol::Voltage;
use crate::spi::{FlashGeometry, JedecId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// Standard page program (0x02), 4 KiB sector erase.
    Standard,
    /// SST25VF: AAI word programming, no page program.
    Sst25Aai,
    /// SST26VF: needs global block unlock.
    Sst26,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chip {
    pub vendor: &'static str,
    pub name: &'static str,
    pub id: u32,
    pub size: u32,
    pub voltage: Voltage,
    pub family: Family,
}

impl Chip {
    pub fn geometry(&self) -> FlashGeometry {
        FlashGeometry {
            size: self.size,
            page_size: 256,
            sector_size: 4096,
            aai: self.family == Family::Sst25Aai,
            global_unlock: self.family == Family::Sst26,
        }
    }

    pub fn display_name(&self) -> String {
        format!("{} {}", self.vendor, self.name)
    }
}

const K: u32 = 1024;
const M: u32 = 1024 * 1024;

use Family::*;
use Voltage::*;

macro_rules! chips {
    ($($v:literal $n:literal $id:literal $sz:expr, $volt:ident $fam:ident;)*) => {
        &[$(Chip { vendor: $v, name: $n, id: $id, size: $sz, voltage: $volt, family: $fam }),*]
    };
}

pub static CHIPS: &[Chip] = chips! {
    // Winbond
    "Winbond" "W25X10" 0xEF3011 128*K, V3_3 Standard;
    "Winbond" "W25X20" 0xEF3012 256*K, V3_3 Standard;
    "Winbond" "W25X40" 0xEF3013 512*K, V3_3 Standard;
    "Winbond" "W25X80" 0xEF3014 M, V3_3 Standard;
    "Winbond" "W25X16" 0xEF3015 2*M, V3_3 Standard;
    "Winbond" "W25X32" 0xEF3016 4*M, V3_3 Standard;
    "Winbond" "W25X64" 0xEF3017 8*M, V3_3 Standard;
    "Winbond" "W25Q10" 0xEF4011 128*K, V3_3 Standard;
    "Winbond" "W25Q20" 0xEF4012 256*K, V3_3 Standard;
    "Winbond" "W25Q40" 0xEF4013 512*K, V3_3 Standard;
    "Winbond" "W25Q80" 0xEF4014 M, V3_3 Standard;
    "Winbond" "W25Q16" 0xEF4015 2*M, V3_3 Standard;
    "Winbond" "W25Q32" 0xEF4016 4*M, V3_3 Standard;
    "Winbond" "W25Q64" 0xEF4017 8*M, V3_3 Standard;
    "Winbond" "W25Q128" 0xEF4018 16*M, V3_3 Standard;
    "Winbond" "W25Q256" 0xEF4019 32*M, V3_3 Standard;
    "Winbond" "W25Q512" 0xEF4020 64*M, V3_3 Standard;
    "Winbond" "W25Q16JV-IM" 0xEF7015 2*M, V3_3 Standard;
    "Winbond" "W25Q32JV-IM" 0xEF7016 4*M, V3_3 Standard;
    "Winbond" "W25Q64JV-IM" 0xEF7017 8*M, V3_3 Standard;
    "Winbond" "W25Q128JV-IM" 0xEF7018 16*M, V3_3 Standard;
    "Winbond" "W25Q256JV-IM" 0xEF7019 32*M, V3_3 Standard;
    "Winbond" "W25Q80DW" 0xEF6014 M, V1_8 Standard;
    "Winbond" "W25Q16FW" 0xEF6015 2*M, V1_8 Standard;
    "Winbond" "W25Q32FW" 0xEF6016 4*M, V1_8 Standard;
    "Winbond" "W25Q64FW" 0xEF6017 8*M, V1_8 Standard;
    "Winbond" "W25Q128FW" 0xEF6018 16*M, V1_8 Standard;
    "Winbond" "W25Q256JW" 0xEF6019 32*M, V1_8 Standard;
    "Winbond" "W25Q16JW-IM" 0xEF8015 2*M, V1_8 Standard;
    "Winbond" "W25Q32JW-IM" 0xEF8016 4*M, V1_8 Standard;
    "Winbond" "W25Q64JW-IM" 0xEF8017 8*M, V1_8 Standard;
    "Winbond" "W25Q128JW-IM" 0xEF8018 16*M, V1_8 Standard;
    // Macronix
    "Macronix" "MX25L512" 0xC22010 64*K, V3_3 Standard;
    "Macronix" "MX25L1005" 0xC22011 128*K, V3_3 Standard;
    "Macronix" "MX25L2005" 0xC22012 256*K, V3_3 Standard;
    "Macronix" "MX25L4005" 0xC22013 512*K, V3_3 Standard;
    "Macronix" "MX25L8005" 0xC22014 M, V3_3 Standard;
    "Macronix" "MX25L1605" 0xC22015 2*M, V3_3 Standard;
    "Macronix" "MX25L3205" 0xC22016 4*M, V3_3 Standard;
    "Macronix" "MX25L6405" 0xC22017 8*M, V3_3 Standard;
    "Macronix" "MX25L12805" 0xC22018 16*M, V3_3 Standard;
    "Macronix" "MX25L25635" 0xC22019 32*M, V3_3 Standard;
    "Macronix" "MX25L51245" 0xC2201A 64*M, V3_3 Standard;
    "Macronix" "MX25U4035" 0xC22533 512*K, V1_8 Standard;
    "Macronix" "MX25U8035" 0xC22534 M, V1_8 Standard;
    "Macronix" "MX25U1635" 0xC22535 2*M, V1_8 Standard;
    "Macronix" "MX25U3235" 0xC22536 4*M, V1_8 Standard;
    "Macronix" "MX25U6435" 0xC22537 8*M, V1_8 Standard;
    "Macronix" "MX25U12835" 0xC22538 16*M, V1_8 Standard;
    "Macronix" "MX25U25635" 0xC22539 32*M, V1_8 Standard;
    "Macronix" "MX25U51245" 0xC2253A 64*M, V1_8 Standard;
    "Macronix" "MX25R8035" 0xC22814 M, V3_3 Standard;
    "Macronix" "MX25R1635" 0xC22815 2*M, V3_3 Standard;
    "Macronix" "MX25R3235" 0xC22816 4*M, V3_3 Standard;
    "Macronix" "MX25R6435" 0xC22817 8*M, V3_3 Standard;
    // GigaDevice
    "GigaDevice" "GD25Q10" 0xC84011 128*K, V3_3 Standard;
    "GigaDevice" "GD25Q20" 0xC84012 256*K, V3_3 Standard;
    "GigaDevice" "GD25Q40" 0xC84013 512*K, V3_3 Standard;
    "GigaDevice" "GD25Q80" 0xC84014 M, V3_3 Standard;
    "GigaDevice" "GD25Q16" 0xC84015 2*M, V3_3 Standard;
    "GigaDevice" "GD25Q32" 0xC84016 4*M, V3_3 Standard;
    "GigaDevice" "GD25Q64" 0xC84017 8*M, V3_3 Standard;
    "GigaDevice" "GD25Q128" 0xC84018 16*M, V3_3 Standard;
    "GigaDevice" "GD25Q256" 0xC84019 32*M, V3_3 Standard;
    "GigaDevice" "GD25LQ80" 0xC86014 M, V1_8 Standard;
    "GigaDevice" "GD25LQ16" 0xC86015 2*M, V1_8 Standard;
    "GigaDevice" "GD25LQ32" 0xC86016 4*M, V1_8 Standard;
    "GigaDevice" "GD25LQ64" 0xC86017 8*M, V1_8 Standard;
    "GigaDevice" "GD25LQ128" 0xC86018 16*M, V1_8 Standard;
    // Spansion / Cypress / Infineon
    "Spansion" "S25FL116K" 0x014015 2*M, V3_3 Standard;
    "Spansion" "S25FL132K" 0x014016 4*M, V3_3 Standard;
    "Spansion" "S25FL164K" 0x014017 8*M, V3_3 Standard;
    "Spansion" "S25FL032P" 0x010215 4*M, V3_3 Standard;
    "Spansion" "S25FL064P" 0x010216 8*M, V3_3 Standard;
    "Spansion" "S25FL128P/S" 0x012018 16*M, V3_3 Standard;
    "Spansion" "S25FL256S" 0x010219 32*M, V3_3 Standard;
    "Spansion" "S25FL512S" 0x010220 64*M, V3_3 Standard;
    // Micron / Numonyx / ST
    "Micron" "M25P05" 0x202010 64*K, V3_3 Standard;
    "Micron" "M25P10" 0x202011 128*K, V3_3 Standard;
    "Micron" "M25P20" 0x202012 256*K, V3_3 Standard;
    "Micron" "M25P40" 0x202013 512*K, V3_3 Standard;
    "Micron" "M25P80" 0x202014 M, V3_3 Standard;
    "Micron" "M25P16" 0x202015 2*M, V3_3 Standard;
    "Micron" "M25P32" 0x202016 4*M, V3_3 Standard;
    "Micron" "M25P64" 0x202017 8*M, V3_3 Standard;
    "Micron" "M25P128" 0x202018 16*M, V3_3 Standard;
    "Micron" "N25Q032A" 0x20BA16 4*M, V3_3 Standard;
    "Micron" "N25Q064A" 0x20BA17 8*M, V3_3 Standard;
    "Micron" "N25Q128A" 0x20BA18 16*M, V3_3 Standard;
    "Micron" "N25Q256A/MT25QL256" 0x20BA19 32*M, V3_3 Standard;
    "Micron" "N25Q512A/MT25QL512" 0x20BA20 64*M, V3_3 Standard;
    "Micron" "N25Q128A (1.8V)" 0x20BB18 16*M, V1_8 Standard;
    "Micron" "MT25QU256" 0x20BB19 32*M, V1_8 Standard;
    "Micron" "MT25QU512" 0x20BB20 64*M, V1_8 Standard;
    // XMC
    "XMC" "XM25QH16" 0x207015 2*M, V3_3 Standard;
    "XMC" "XM25QH32" 0x207016 4*M, V3_3 Standard;
    "XMC" "XM25QH64" 0x207017 8*M, V3_3 Standard;
    "XMC" "XM25QH128" 0x207018 16*M, V3_3 Standard;
    "XMC" "XM25QH256" 0x207019 32*M, V3_3 Standard;
    // EON / ESMT
    "EON" "EN25F80" 0x1C3114 M, V3_3 Standard;
    "EON" "EN25F16" 0x1C3115 2*M, V3_3 Standard;
    "EON" "EN25F32" 0x1C3116 4*M, V3_3 Standard;
    "EON" "EN25Q16" 0x1C3015 2*M, V3_3 Standard;
    "EON" "EN25Q32" 0x1C3016 4*M, V3_3 Standard;
    "EON" "EN25Q64" 0x1C3017 8*M, V3_3 Standard;
    "EON" "EN25Q128" 0x1C3018 16*M, V3_3 Standard;
    "EON" "EN25QH16" 0x1C7015 2*M, V3_3 Standard;
    "EON" "EN25QH32" 0x1C7016 4*M, V3_3 Standard;
    "EON" "EN25QH64" 0x1C7017 8*M, V3_3 Standard;
    "EON" "EN25QH128" 0x1C7018 16*M, V3_3 Standard;
    // ISSI
    "ISSI" "IS25LP016" 0x9D6015 2*M, V3_3 Standard;
    "ISSI" "IS25LP032" 0x9D6016 4*M, V3_3 Standard;
    "ISSI" "IS25LP064" 0x9D6017 8*M, V3_3 Standard;
    "ISSI" "IS25LP128" 0x9D6018 16*M, V3_3 Standard;
    "ISSI" "IS25LP256" 0x9D6019 32*M, V3_3 Standard;
    "ISSI" "IS25WP016" 0x9D7015 2*M, V1_8 Standard;
    "ISSI" "IS25WP032" 0x9D7016 4*M, V1_8 Standard;
    "ISSI" "IS25WP064" 0x9D7017 8*M, V1_8 Standard;
    "ISSI" "IS25WP128" 0x9D7018 16*M, V1_8 Standard;
    "ISSI" "IS25WP256" 0x9D7019 32*M, V1_8 Standard;
    // Puya, Zbit, Boya, Fudan
    "Puya" "P25Q80H" 0x856014 M, V3_3 Standard;
    "Puya" "P25Q16H" 0x856015 2*M, V3_3 Standard;
    "Puya" "P25Q32H" 0x856016 4*M, V3_3 Standard;
    "Puya" "P25Q64H" 0x856017 8*M, V3_3 Standard;
    "Puya" "P25Q128H" 0x856018 16*M, V3_3 Standard;
    "Zbit" "ZB25VQ16" 0x5E4015 2*M, V3_3 Standard;
    "Zbit" "ZB25VQ32" 0x5E4016 4*M, V3_3 Standard;
    "Zbit" "ZB25VQ64" 0x5E4017 8*M, V3_3 Standard;
    "Zbit" "ZB25VQ128" 0x5E4018 16*M, V3_3 Standard;
    "BoyaMicro" "BY25Q16" 0x684015 2*M, V3_3 Standard;
    "BoyaMicro" "BY25Q32" 0x684016 4*M, V3_3 Standard;
    "BoyaMicro" "BY25Q64" 0x684017 8*M, V3_3 Standard;
    "BoyaMicro" "BY25Q128" 0x684018 16*M, V3_3 Standard;
    "Fudan" "FM25Q16" 0xA14015 2*M, V3_3 Standard;
    "Fudan" "FM25Q32" 0xA14016 4*M, V3_3 Standard;
    "Fudan" "FM25Q64" 0xA14017 8*M, V3_3 Standard;
    "Fudan" "FM25Q128" 0xA14018 16*M, V3_3 Standard;
    // Atmel / Adesto / Renesas
    "Adesto" "AT25DF041A" 0x1F4401 512*K, V3_3 Standard;
    "Adesto" "AT25DF081A" 0x1F4501 M, V3_3 Standard;
    "Adesto" "AT25DF161" 0x1F4602 2*M, V3_3 Standard;
    "Adesto" "AT25DF321A" 0x1F4701 4*M, V3_3 Standard;
    "Adesto" "AT25SF041" 0x1F8401 512*K, V3_3 Standard;
    "Adesto" "AT25SF081" 0x1F8501 M, V3_3 Standard;
    "Adesto" "AT25SF161" 0x1F8601 2*M, V3_3 Standard;
    "Adesto" "AT25SF321" 0x1F8701 4*M, V3_3 Standard;
    // SST / Microchip
    "SST" "SST25VF040B" 0xBF258D 512*K, V3_3 Sst25Aai;
    "SST" "SST25VF080B" 0xBF258E M, V3_3 Sst25Aai;
    "SST" "SST25VF016B" 0xBF2541 2*M, V3_3 Sst25Aai;
    "SST" "SST25VF032B" 0xBF254A 4*M, V3_3 Sst25Aai;
    "SST" "SST25VF064C" 0xBF254B 8*M, V3_3 Standard;
    "SST" "SST26VF016B" 0xBF2641 2*M, V3_3 Sst26;
    "SST" "SST26VF032B" 0xBF2642 4*M, V3_3 Sst26;
    "SST" "SST26VF064B" 0xBF2643 8*M, V3_3 Sst26;
};

pub fn manufacturer_name(code: u8) -> &'static str {
    match code {
        0x01 => "Spansion",
        0x1C => "EON",
        0x1F => "Adesto",
        0x20 => "Micron/XMC",
        0x5E => "Zbit",
        0x68 => "BoyaMicro",
        0x85 => "Puya",
        0x8C => "ESMT",
        0x9D => "ISSI",
        0xA1 => "Fudan",
        0xBF => "SST",
        0xC2 => "Macronix",
        0xC8 => "GigaDevice",
        0xEF => "Winbond",
        _ => "Unknown",
    }
}

pub fn lookup(id: JedecId) -> Option<&'static Chip> {
    CHIPS.iter().find(|c| c.id == id.as_u32())
}

/// Case-insensitive substring search over vendor and part name.
pub fn search(q: &str) -> Vec<&'static Chip> {
    let q = q.to_ascii_lowercase();
    CHIPS
        .iter()
        .filter(|c| {
            q.is_empty()
                || c.name.to_ascii_lowercase().contains(&q)
                || c.vendor.to_ascii_lowercase().contains(&q)
        })
        .collect()
}

/// Best guess of capacity from the third ID byte (common 2^n encoding).
pub fn size_from_id(id: JedecId) -> Option<u32> {
    let c = id.0[2];
    match c {
        0x10..=0x1F => Some(1u32 << c),
        0x20..=0x22 => Some(1u32 << (c - 0x20 + 26)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_unique() {
        let mut ids: Vec<u32> = CHIPS.iter().map(|c| c.id).collect();
        ids.sort();
        let n = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), n);
    }

    #[test]
    fn capacity_byte_matches_table() {
        for c in CHIPS {
            if let Some(s) =
                size_from_id(JedecId([(c.id >> 16) as u8, (c.id >> 8) as u8, c.id as u8]))
            {
                if c.vendor != "Adesto" && c.vendor != "SST" && c.vendor != "Spansion" {
                    assert_eq!(s, c.size, "{}", c.name);
                }
            }
        }
    }
}
