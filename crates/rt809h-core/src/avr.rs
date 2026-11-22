//! Atmel/Microchip AVR microcontrollers over the ISP header (SPI serial
//! programming, as described in the "Serial Downloading" chapter of the AVR
//! datasheets). Newer parts with a UPDI pin are handled by [`crate::updi`]
//! on the same lines.
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
    /// how the part is programmed
    pub bus: Bus,
}

/// Programming interface of an AVR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bus {
    /// SPI serial programming (MOSI, MISO, SCK, RESET)
    Isp,
    /// one-wire UPDI
    Updi(UpdiFamily),
}

/// UPDI part families (they differ in memory map and NVM controller).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdiFamily {
    /// tinyAVR 0/1/2-series
    Tiny,
    /// megaAVR 0-series (ATmega808 … ATmega4809)
    Mega0,
    /// AVR DA/DB
    Dx,
    /// AVR DD (UPDI pin can be made a GPIO)
    Dd,
    /// AVR DU (USB; word writes like DD, newer NVM controller "P:4")
    Du,
    /// AVR EA (page buffer, NVM controller "P:3")
    Ea,
    /// AVR EB (page buffer, NVM controller "P:5")
    Eb,
}

const fn p(name: &'static str, sig: u32, flash: u32, page: u16, eeprom: u16, fuses: u8) -> AvrPart {
    AvrPart {
        name,
        signature: [(sig >> 16) as u8, (sig >> 8) as u8, sig as u8],
        flash,
        page,
        eeprom,
        fuses,
        bus: Bus::Isp,
    }
}

const fn u(
    name: &'static str,
    sig: u32,
    flash: u32,
    page: u16,
    eeprom: u16,
    fam: UpdiFamily,
) -> AvrPart {
    AvrPart {
        bus: Bus::Updi(fam),
        fuses: crate::updi::FUSES as u8,
        ..p(name, sig, flash, page, eeprom, 0)
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

use UpdiFamily::{Dd, Du, Dx, Ea, Eb, Mega0, Tiny};

/// Parts programmed over UPDI, from the device datasheets.
pub static UPDI_PARTS: &[AvrPart] = &[
    u("ATtiny202", 0x1E9123, 2048, 64, 64, Tiny),
    u("ATtiny204", 0x1E9122, 2048, 64, 64, Tiny),
    u("ATtiny212", 0x1E9121, 2048, 64, 64, Tiny),
    u("ATtiny214", 0x1E9120, 2048, 64, 64, Tiny),
    u("ATtiny402", 0x1E9227, 4096, 64, 128, Tiny),
    u("ATtiny404", 0x1E9226, 4096, 64, 128, Tiny),
    u("ATtiny406", 0x1E9225, 4096, 64, 128, Tiny),
    u("ATtiny412", 0x1E9223, 4096, 64, 128, Tiny),
    u("ATtiny414", 0x1E9222, 4096, 64, 128, Tiny),
    u("ATtiny416", 0x1E9221, 4096, 64, 128, Tiny),
    u("ATtiny417", 0x1E9220, 4096, 64, 128, Tiny),
    u("ATtiny424", 0x1E922C, 4096, 64, 128, Tiny),
    u("ATtiny426", 0x1E922B, 4096, 64, 128, Tiny),
    u("ATtiny427", 0x1E922A, 4096, 64, 128, Tiny),
    u("ATtiny804", 0x1E9325, 8192, 64, 128, Tiny),
    u("ATtiny806", 0x1E9324, 8192, 64, 128, Tiny),
    u("ATtiny807", 0x1E9323, 8192, 64, 128, Tiny),
    u("ATtiny814", 0x1E9322, 8192, 64, 128, Tiny),
    u("ATtiny816", 0x1E9321, 8192, 64, 128, Tiny),
    u("ATtiny817", 0x1E9320, 8192, 64, 128, Tiny),
    u("ATtiny824", 0x1E9329, 8192, 64, 128, Tiny),
    u("ATtiny826", 0x1E9328, 8192, 64, 128, Tiny),
    u("ATtiny827", 0x1E9327, 8192, 64, 128, Tiny),
    u("ATtiny1604", 0x1E9425, 16384, 64, 256, Tiny),
    u("ATtiny1606", 0x1E9424, 16384, 64, 256, Tiny),
    u("ATtiny1607", 0x1E9423, 16384, 64, 256, Tiny),
    u("ATtiny1614", 0x1E9422, 16384, 64, 256, Tiny),
    u("ATtiny1616", 0x1E9421, 16384, 64, 256, Tiny),
    u("ATtiny1617", 0x1E9420, 16384, 64, 256, Tiny),
    u("ATtiny1624", 0x1E942A, 16384, 64, 256, Tiny),
    u("ATtiny1626", 0x1E9429, 16384, 64, 256, Tiny),
    u("ATtiny1627", 0x1E9428, 16384, 64, 256, Tiny),
    u("ATtiny3216", 0x1E9521, 32768, 128, 256, Tiny),
    u("ATtiny3217", 0x1E9522, 32768, 128, 256, Tiny),
    u("ATtiny3224", 0x1E9528, 32768, 128, 256, Tiny),
    u("ATtiny3226", 0x1E9527, 32768, 128, 256, Tiny),
    u("ATtiny3227", 0x1E9526, 32768, 128, 256, Tiny),
    u("ATmega808", 0x1E9326, 8192, 64, 256, Mega0),
    u("ATmega809", 0x1E932A, 8192, 64, 256, Mega0),
    u("ATmega1608", 0x1E9427, 16384, 64, 256, Mega0),
    u("ATmega1609", 0x1E9426, 16384, 64, 256, Mega0),
    u("ATmega3208", 0x1E9530, 32768, 128, 256, Mega0),
    u("ATmega3209", 0x1E9531, 32768, 128, 256, Mega0),
    u("ATmega4808", 0x1E9650, 49152, 128, 256, Mega0),
    u("ATmega4809", 0x1E9651, 49152, 128, 256, Mega0),
    u("AVR32DA28", 0x1E9534, 32768, 512, 512, Dx),
    u("AVR32DA32", 0x1E9533, 32768, 512, 512, Dx),
    u("AVR32DA48", 0x1E9532, 32768, 512, 512, Dx),
    u("AVR64DA28", 0x1E9615, 65536, 512, 512, Dx),
    u("AVR64DA32", 0x1E9614, 65536, 512, 512, Dx),
    u("AVR64DA48", 0x1E9613, 65536, 512, 512, Dx),
    u("AVR64DA64", 0x1E9612, 65536, 512, 512, Dx),
    u("AVR128DA28", 0x1E970A, 131072, 512, 512, Dx),
    u("AVR128DA32", 0x1E9709, 131072, 512, 512, Dx),
    u("AVR128DA48", 0x1E9708, 131072, 512, 512, Dx),
    u("AVR128DA64", 0x1E9707, 131072, 512, 512, Dx),
    u("AVR32DB28", 0x1E9537, 32768, 512, 512, Dx),
    u("AVR32DB32", 0x1E9536, 32768, 512, 512, Dx),
    u("AVR32DB48", 0x1E9535, 32768, 512, 512, Dx),
    u("AVR64DB28", 0x1E9619, 65536, 512, 512, Dx),
    u("AVR64DB32", 0x1E9618, 65536, 512, 512, Dx),
    u("AVR64DB48", 0x1E9617, 65536, 512, 512, Dx),
    u("AVR64DB64", 0x1E9616, 65536, 512, 512, Dx),
    u("AVR128DB28", 0x1E970E, 131072, 512, 512, Dx),
    u("AVR128DB32", 0x1E970D, 131072, 512, 512, Dx),
    u("AVR128DB48", 0x1E970C, 131072, 512, 512, Dx),
    u("AVR128DB64", 0x1E970B, 131072, 512, 512, Dx),
    u("AVR16DD14", 0x1E9434, 16384, 512, 256, Dd),
    u("AVR16DD20", 0x1E9433, 16384, 512, 256, Dd),
    u("AVR16DD28", 0x1E9432, 16384, 512, 256, Dd),
    u("AVR16DD32", 0x1E9431, 16384, 512, 256, Dd),
    u("AVR32DD14", 0x1E953B, 32768, 512, 256, Dd),
    u("AVR32DD20", 0x1E953A, 32768, 512, 256, Dd),
    u("AVR32DD28", 0x1E9539, 32768, 512, 256, Dd),
    u("AVR32DD32", 0x1E9538, 32768, 512, 256, Dd),
    u("AVR64DD14", 0x1E961D, 65536, 512, 256, Dd),
    u("AVR64DD20", 0x1E961C, 65536, 512, 256, Dd),
    u("AVR64DD28", 0x1E961B, 65536, 512, 256, Dd),
    u("AVR64DD32", 0x1E961A, 65536, 512, 256, Dd),
    // AVR DA with "S" suffix (same as DA)
    u("AVR32DA28S", 0x1E9572, 32768, 512, 512, Dx),
    u("AVR32DA32S", 0x1E9571, 32768, 512, 512, Dx),
    u("AVR32DA48S", 0x1E9570, 32768, 512, 512, Dx),
    u("AVR64DA28S", 0x1E962E, 65536, 512, 512, Dx),
    u("AVR64DA32S", 0x1E962D, 65536, 512, 512, Dx),
    u("AVR64DA48S", 0x1E962C, 65536, 512, 512, Dx),
    u("AVR64DA64S", 0x1E962B, 65536, 512, 512, Dx),
    u("AVR128DA28S", 0x1E9712, 131072, 512, 512, Dx),
    u("AVR128DA32S", 0x1E9711, 131072, 512, 512, Dx),
    u("AVR128DA48S", 0x1E9710, 131072, 512, 512, Dx),
    u("AVR128DA64S", 0x1E970F, 131072, 512, 512, Dx),
    // AVR DU
    u("AVR16DU14", 0x1E943B, 16384, 512, 256, Du),
    u("AVR16DU20", 0x1E943A, 16384, 512, 256, Du),
    u("AVR16DU28", 0x1E9439, 16384, 512, 256, Du),
    u("AVR16DU32", 0x1E9438, 16384, 512, 256, Du),
    u("AVR32DU14", 0x1E954F, 32768, 512, 256, Du),
    u("AVR32DU20", 0x1E954E, 32768, 512, 256, Du),
    u("AVR32DU28", 0x1E9540, 32768, 512, 256, Du),
    u("AVR32DU32", 0x1E953F, 32768, 512, 256, Du),
    u("AVR64DU28", 0x1E9622, 65536, 512, 256, Du),
    u("AVR64DU32", 0x1E9621, 65536, 512, 256, Du),
    // AVR EA
    u("AVR8EA28", 0x1E932C, 8192, 64, 512, Ea),
    u("AVR8EA32", 0x1E932B, 8192, 64, 512, Ea),
    u("AVR16EA28", 0x1E9437, 16384, 64, 512, Ea),
    u("AVR16EA32", 0x1E9436, 16384, 64, 512, Ea),
    u("AVR16EA48", 0x1E9435, 16384, 64, 512, Ea),
    u("AVR32EA28", 0x1E953E, 32768, 64, 512, Ea),
    u("AVR32EA32", 0x1E953D, 32768, 64, 512, Ea),
    u("AVR32EA48", 0x1E953C, 32768, 64, 512, Ea),
    u("AVR64EA28", 0x1E9620, 65536, 128, 512, Ea),
    u("AVR64EA32", 0x1E961F, 65536, 128, 512, Ea),
    u("AVR64EA48", 0x1E961E, 65536, 128, 512, Ea),
    // AVR EB
    u("AVR16EB14", 0x1E9449, 16384, 64, 512, Eb),
    u("AVR16EB20", 0x1E9440, 16384, 64, 512, Eb),
    u("AVR16EB28", 0x1E943F, 16384, 64, 512, Eb),
    u("AVR16EB32", 0x1E943E, 16384, 64, 512, Eb),
    u("AVR32EB14", 0x1E952D, 32768, 64, 512, Eb),
    u("AVR32EB20", 0x1E952C, 32768, 64, 512, Eb),
    u("AVR32EB28", 0x1E952B, 32768, 64, 512, Eb),
    u("AVR32EB32", 0x1E952A, 32768, 64, 512, Eb),
];

/// Every known AVR, ISP parts first.
pub fn all_parts() -> impl Iterator<Item = &'static AvrPart> {
    PARTS.iter().chain(UPDI_PARTS)
}

/// Part by name (case-insensitive; a package suffix after `@` is ignored).
pub fn find(name: &str) -> Option<&'static AvrPart> {
    let base = name.split('@').next().unwrap_or(name).trim();
    all_parts().find(|p| p.name.eq_ignore_ascii_case(base))
}

/// Names of the parts with this signature.
pub fn by_signature(sig: [u8; 3]) -> Vec<&'static str> {
    all_parts()
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
    pub fn write_eeprom(
        &mut self,
        _part: &AvrPart,
        data: &[u8],
        old: &[u8],
        progress: Progress,
    ) -> Result<()> {
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

/// A part in programming mode, on either interface.
pub enum Link<'a> {
    Isp(Isp<'a>),
    Updi(crate::updi::Updi<'a>),
}

impl<'a> Link<'a> {
    /// Fail unless the part answering is `part`. A locked UPDI part hides
    /// its signature; only its family is checked then.
    pub fn check(&mut self, part: &AvrPart) -> Result<()> {
        if let Link::Updi(u) = self {
            u.check_family(part)?;
            if u.locked() {
                log::info!("{} is locked; only a chip erase works", part.name);
                return Ok(());
            }
        }
        let sig = self.signature()?;
        if sig == part.signature {
            return Ok(());
        }
        let known = by_signature(sig);
        Err(Error::Chip(format!(
            "Signature {:02X} {:02X} {:02X} does not match {} ({})",
            sig[0],
            sig[1],
            sig[2],
            part.name,
            if known.is_empty() {
                "unknown part".to_string()
            } else {
                format!("found {}", known.join(" / "))
            }
        )))
    }

    pub fn isp(&mut self) -> Result<&mut Isp<'a>> {
        match self {
            Link::Isp(i) => Ok(i),
            Link::Updi(_) => Err(Error::Chip("This part is programmed over UPDI".into())),
        }
    }

    pub fn updi(&mut self) -> Result<&mut crate::updi::Updi<'a>> {
        match self {
            Link::Updi(u) => Ok(u),
            Link::Isp(_) => Err(Error::Chip("This part is programmed over ISP".into())),
        }
    }

    pub fn signature(&mut self) -> Result<[u8; 3]> {
        match self {
            Link::Isp(i) => i.signature(),
            Link::Updi(u) => u.signature(),
        }
    }

    pub fn chip_erase(&mut self) -> Result<()> {
        match self {
            Link::Isp(i) => i.chip_erase(),
            Link::Updi(u) => u.chip_erase(),
        }
    }

    pub fn read_flash(&mut self, part: &AvrPart, progress: Progress) -> Result<Vec<u8>> {
        match self {
            Link::Isp(i) => i.read_flash(part, progress),
            Link::Updi(u) => u.read_flash(part, progress),
        }
    }

    pub fn read_eeprom(&mut self, part: &AvrPart, progress: Progress) -> Result<Vec<u8>> {
        match self {
            Link::Isp(i) => i.read_eeprom(part, progress),
            Link::Updi(u) => u.read_eeprom(part, progress),
        }
    }

    pub fn write_flash(&mut self, part: &AvrPart, data: &[u8], progress: Progress) -> Result<()> {
        match self {
            Link::Isp(i) => i.write_flash(part, data, progress),
            Link::Updi(u) => u.write_flash(part, data, progress),
        }
    }

    pub fn write_eeprom(
        &mut self,
        part: &AvrPart,
        data: &[u8],
        old: &[u8],
        progress: Progress,
    ) -> Result<()> {
        match self {
            Link::Isp(i) => i.write_eeprom(part, data, old, progress),
            Link::Updi(u) => u.write_eeprom(part, data, old, progress),
        }
    }

    pub fn release(&mut self) -> Result<()> {
        match self {
            Link::Isp(i) => i.release(),
            Link::Updi(u) => u.release(),
        }
    }

    /// All fuse bytes of `part` and its lock state.
    pub fn read_fuses(&mut self, part: &AvrPart) -> Result<FuseValues> {
        let layout = fuse_layout(part);
        let (raw, lock) = match self {
            Link::Isp(i) => {
                let f = i.fuses(part.fuses)?;
                let mut raw = vec![f.low, f.high];
                raw.extend(f.extended);
                (raw, format!("0x{:02X}", f.lock))
            }
            Link::Updi(u) => {
                let raw = u.fuses()?;
                let lock = u.lock_bits()?;
                let open = matches!(lock, 0xC5 | 0x5CC5_C55C);
                let lock = if lock > 0xFF {
                    format!("0x{lock:08X}")
                } else {
                    format!("0x{lock:02X}")
                };
                (
                    raw,
                    if open {
                        format!("{lock} (unlocked)")
                    } else {
                        format!("{lock} (locked)")
                    },
                )
            }
        };
        Ok(FuseValues {
            fuses: layout.into_iter().map(|(i, n)| (i, n, raw[i])).collect(),
            lock,
        })
    }

    /// Program fuse bytes, given as (index from [`fuse_layout`], value).
    pub fn write_fuses(&mut self, part: &AvrPart, values: &[(usize, u8)]) -> Result<()> {
        let layout = fuse_layout(part);
        for &(i, _) in values {
            if !layout.iter().any(|&(j, _)| j == i) {
                return Err(Error::Chip(format!("{} has no fuse {i}", part.name)));
            }
        }
        match self {
            Link::Isp(isp) => {
                let get = |n| values.iter().find(|&&(i, _)| i == n).map(|&(_, v)| v);
                isp.write_fuses(get(0), get(1), get(2), None)
            }
            Link::Updi(u) => {
                for &(i, v) in values {
                    u.write_fuse(part, i, v)?;
                }
                Ok(())
            }
        }
    }
}

/// Fuse bytes read from a part: (index, name, value) and the lock state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuseValues {
    pub fuses: Vec<(usize, &'static str, u8)>,
    pub lock: String,
}

/// The fuse bytes of `part` as (index, name). The index is what
/// [`Link::write_fuses`] takes.
pub fn fuse_layout(part: &AvrPart) -> Vec<(usize, &'static str)> {
    match part.bus {
        Bus::Isp => {
            let names: &[&str] = if part.fuses > 2 {
                &["Low", "High", "Extended"]
            } else {
                &["Low", "High"]
            };
            names.iter().copied().enumerate().collect()
        }
        Bus::Updi(f) => {
            let mut v = vec![(0, "WDTCFG"), (1, "BODCFG"), (2, "OSCCFG")];
            if f == UpdiFamily::Tiny {
                v.push((4, "TCD0CFG"));
            }
            v.extend([(5, "SYSCFG0"), (6, "SYSCFG1")]);
            match f {
                UpdiFamily::Tiny | UpdiFamily::Mega0 => v.extend([(7, "APPEND"), (8, "BOOTEND")]),
                _ => v.extend([(7, "CODESIZE"), (8, "BOOTSIZE")]),
            }
            v
        }
    }
}

fn plausible(sig: [u8; 3]) -> bool {
    sig[0] == 0x1E && sig != [0x1E, 0xFF, 0xFF] && sig != [0x1E, 0x00, 0x00]
}

#[cfg(test)]
mod tests {
    #[test]
    fn newer_updi_families() {
        let fam = |n| match find(n).unwrap().bus {
            Bus::Updi(f) => f,
            Bus::Isp => panic!("{n} is not UPDI"),
        };
        assert_eq!(fam("AVR16EA28"), UpdiFamily::Ea);
        assert_eq!(fam("AVR32EB32"), UpdiFamily::Eb);
        assert_eq!(fam("AVR64DU32"), UpdiFamily::Du);
        assert_eq!(fam("AVR128DA48S"), UpdiFamily::Dx);
        assert_eq!(by_signature([0x1E, 0x94, 0x37]), ["AVR16EA28"]);
    }

    #[test]
    fn fuse_layouts() {
        let names = |n| -> Vec<&str> {
            fuse_layout(find(n).unwrap())
                .into_iter()
                .map(|f| f.1)
                .collect()
        };
        assert_eq!(names("ATmega328P"), ["Low", "High", "Extended"]);
        assert_eq!(
            names("ATtiny212"),
            ["WDTCFG", "BODCFG", "OSCCFG", "TCD0CFG", "SYSCFG0", "SYSCFG1", "APPEND", "BOOTEND"]
        );
        // index 3 is reserved on every UPDI part
        for p in all_parts().filter(|p| p.bus != Bus::Isp) {
            assert!(fuse_layout(p)
                .iter()
                .all(|f| f.0 != 3 && f.0 < crate::updi::FUSES));
        }
    }

    use super::*;

    #[test]
    fn table() {
        let p = find("ATMEGA328P@DIP28").unwrap();
        assert_eq!(p.signature, [0x1E, 0x95, 0x0F]);
        assert_eq!(by_signature([0x1E, 0x93, 0x0B]), vec!["ATtiny85"]);
        for p in all_parts() {
            assert_eq!(p.flash % p.page as u32, 0, "{}", p.name);
            assert!(!by_signature(p.signature).is_empty());
        }
        let t = find("attiny1614").unwrap();
        assert_eq!(t.bus, Bus::Updi(UpdiFamily::Tiny));
        assert_eq!(by_signature([0x1E, 0x94, 0x22]), vec!["ATtiny1614"]);
        // UPDI signatures never collide with ISP parts
        for u in UPDI_PARTS {
            assert!(
                PARTS.iter().all(|p| p.signature != u.signature),
                "{}",
                u.name
            );
        }
    }
}
