//! Chip families that run on a logic image inside the programmer
//! (Microwire, parallel flash, ...).
//!
//! The images are part of the vendor's Windows software (`LIB/ALGO_*.DAT`)
//! and are not distributed with this project. They are looked up in the
//! user's own copy of that software: `$RT809H_ALGO_DIR`,
//! `~/.local/share/rt809h/algos`, or a Wine installation.

use std::path::PathBuf;
use std::time::Duration;

use crate::ftdi::BitMode;
use crate::protocol::{Mcu, CMD_LOAD_ALGO, CMD_SET_RAILS};
use crate::spi::Progress;
use crate::{Error, Result};

/// Frame tail used while a logic image is active; the first byte is set
/// per image (see [`Profile::tail0`]).
pub const fn tail(t0: u8) -> [u8; 5] {
    [t0, 0x00, 0x00, 0x00, 0x1E]
}
pub const TAIL: [u8; 5] = tail(0xEE);
const TAIL_RAILS: [u8; 5] = [0xCD; 5];
const BLOCK: usize = 32 * 1024;

pub const CMD_PARAMS: u8 = 0x52;
pub const CMD_READ_ID: u8 = 0x61;
pub const CMD_READ: u8 = 0x65;
pub const CMD_PROGRAM: u8 = 0x7A;

/// Directories searched for logic images.
pub fn search_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(d) = std::env::var("RT809H_ALGO_DIR") {
        v.push(PathBuf::from(d));
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        v.push(home.join(".local/share/rt809h/algos"));
        for prefix in [".wine/drive_c", ".local/share/rt809h-wine/ddrive"] {
            for sub in [
                "RT809H",
                "Program Files/RT809H",
                "Program Files (x86)/RT809H",
            ] {
                v.push(home.join(prefix).join(sub).join("LIB"));
            }
        }
    }
    v
}

/// Find `NAME` (without `.DAT`) in the search directories.
pub fn find_image(name: &str) -> Result<PathBuf> {
    let want = format!("{name}.DAT").to_ascii_uppercase();
    for d in search_dirs() {
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().to_ascii_uppercase() == want {
                    return Ok(e.path());
                }
            }
        }
    }
    Err(Error::Chip(format!(
        "This chip needs the programmer logic image {name}.DAT from the RT809H \
         Windows software. Copy its LIB folder to ~/.local/share/rt809h/algos \
         (or set RT809H_ALGO_DIR)."
    )))
}

/// Pad an image to whole 32 KiB blocks.
pub fn blocks(image: &[u8]) -> Vec<u8> {
    let mut v = image.to_vec();
    let n = v.len().div_ceil(BLOCK).max(1) * BLOCK;
    v.resize(n, 0xFF);
    v
}

impl Mcu {
    /// Upload a logic image (the MCU must be running `BASE`).
    pub fn load_image(&mut self, image: &[u8]) -> Result<()> {
        let data = blocks(image);
        let n = (data.len() / BLOCK) as u8;
        self.command(CMD_LOAD_ALGO, 0x55, 0xAA, [0xCD, 0xCD, 0xCD, 0xCD, n])?;
        let prev = self.ftdi.read_timeout;
        self.ftdi.read_timeout = Duration::from_secs(5);
        let r = (|| {
            for b in data.chunks(BLOCK) {
                self.ftdi.write(b)?;
                self.ftdi.read_exact(1)?;
            }
            Ok(())
        })();
        self.ftdi.read_timeout = prev;
        r
    }

    /// Rails in image mode: same layout as `56` in BASE mode.
    pub fn set_rails_words(&mut self, words: &[u16; 20]) -> Result<()> {
        let payload: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        self.command(CMD_SET_RAILS, 0x14, 0x00, TAIL_RAILS)?;
        self.ftdi.write(&payload)?;
        self.ftdi.read_exact(40)?;
        Ok(())
    }

    /// Send the 256-byte chip parameter block; returns the status byte.
    pub fn set_params(&mut self, block: &[u8; 256], tail: [u8; 5]) -> Result<u8> {
        self.command(CMD_PARAMS, 0x00, 0x00, tail)?;
        self.ftdi.write(block)?;
        Ok(self.ftdi.read_exact(1)?[0])
    }

    /// Stream `len` bytes for a read-type command.
    pub fn image_read(
        &mut self,
        cmd: u8,
        tail: [u8; 5],
        len: usize,
        progress: Progress,
    ) -> Result<Vec<u8>> {
        self.command(cmd, 0x00, 0x00, tail)?;
        let prev = self.ftdi.read_timeout;
        self.ftdi.read_timeout = Duration::from_secs(10);
        let mut out = Vec::with_capacity(len);
        let r = (|| {
            while out.len() < len {
                let n = (len - out.len()).min(65536);
                out.extend(self.ftdi.read_exact(n)?);
                if !progress(out.len() as u64, len as u64) {
                    return Err(Error::Cancelled);
                }
            }
            Ok(())
        })();
        self.ftdi.read_timeout = prev;
        r.map(|_| out)
    }

    /// Program the whole chip; the MCU answers `AA 55` when done.
    pub fn image_program(&mut self, tail: [u8; 5], data: &[u8], progress: Progress) -> Result<()> {
        self.command(CMD_PROGRAM, 0x00, 0x00, tail)?;
        for (i, c) in data.chunks(65536).enumerate() {
            self.ftdi.write(c)?;
            if !progress((i * 65536 + c.len()) as u64, data.len() as u64) {
                return Err(Error::Cancelled);
            }
        }
        let prev = self.ftdi.read_timeout;
        self.ftdi.read_timeout = Duration::from_secs(60);
        let r = self.ftdi.read_exact(2);
        self.ftdi.read_timeout = prev;
        match r?.as_slice() {
            [0xAA, 0x55] => Ok(()),
            other => Err(Error::Chip(format!(
                "programming failed (status {other:02X?})"
            ))),
        }
    }

    /// Start a whole-chip erase and wait for the image's one-byte reply.
    ///
    /// The image waits up to the part's erase timeout (units of 10 ms) and
    /// answers `AA`; the caller should confirm the result with a blank check.
    pub fn image_erase(&mut self, setup: &Setup) -> Result<u8> {
        self.command(nor::CMD_ERASE, 0x00, 0x00, setup.tail())?;
        let wait = Duration::from_millis(setup.t_erase as u64 * 10 + 10_000);
        match self.ftdi.read_timeout(1, wait)?.first() {
            Some(&b) => Ok(b),
            None => Err(Error::Chip("no answer to the erase command".into())),
        }
    }

    /// End of an image-mode operation.
    pub fn image_finish(&mut self) -> Result<()> {
        self.ftdi.set_bitmode(0xFF, BitMode::Mpsse)?;
        self.ftdi.write(&[0x82, 0x00, 0x80])?;
        std::thread::sleep(Duration::from_millis(60));
        self.ftdi.write(&[0x82, 0xFF, 0x00])?;
        self.ftdi.set_bitmode(0xFF, BitMode::Reset)?;
        self.ftdi.reset()?;
        self.ftdi.purge()?;
        Ok(())
    }
}

/// Rail words and flags that are fixed for every part using one logic image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Profile {
    /// rail table words 1, 2, 15 and 18/19 (logic level, control, Vpp, Vcc)
    pub level: u16,
    pub ctrl: u16,
    pub vpp: u16,
    pub vcc: u16,
    /// parameter block u32 at 36 and at 40
    pub flags36: u32,
    pub flags40: u32,
    /// first tail byte of image-mode frames
    pub tail0: u8,
}

/// Everything an image-mode part is driven with: the image profile plus the
/// part's own capacity, timeouts and buffer/page sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Setup {
    pub profile: Profile,
    pub size: u32,
    pub t_erase: u32,
    pub t_program: u32,
    pub buffer: u32,
    /// page / sector field of the part record (0 for most parts)
    pub page: u16,
}

impl Setup {
    pub fn new(profile: Profile, size: u32, t_erase: u32, t_program: u32) -> Self {
        Setup {
            profile,
            size,
            t_erase,
            t_program,
            buffer: 8,
            page: 0,
        }
    }

    /// Frame tail for image-mode commands.
    pub fn tail(&self) -> [u8; 5] {
        tail(self.profile.tail0)
    }

    /// Rail table for `56`.
    pub fn rails(&self) -> [u16; 20] {
        let p = &self.profile;
        let mut w = [0x0211u16; 20];
        w[0] = 0x0010;
        w[1] = p.level;
        w[2] = p.ctrl;
        w[15] = p.vpp;
        w[18] = p.vcc;
        w[19] = p.vcc;
        w
    }

    /// The 256-byte parameter block for `52`.
    pub fn params(&self) -> [u8; 256] {
        let p = &self.profile;
        let mut b = [0u8; 256];
        let mut put = |o: usize, v: &[u8]| b[o..o + v.len()].copy_from_slice(v);
        put(0, &self.size.to_le_bytes());
        for (o, v) in [
            (6, p.level),
            (8, p.ctrl),
            (10, 0x0211),
            (12, p.vpp),
            (14, p.vcc),
        ] {
            put(o, &v.to_le_bytes());
        }
        put(16, &self.t_erase.to_le_bytes());
        put(20, &self.t_program.to_le_bytes());
        put(24, &self.buffer.to_le_bytes());
        put(28, &0x100u16.to_le_bytes());
        put(30, &self.page.to_le_bytes());
        put(32, &0x10_0000u32.to_le_bytes());
        put(36, &p.flags36.to_le_bytes());
        put(40, &p.flags40.to_le_bytes());
        b
    }
}

/// Microwire (93xx) EEPROMs on the `ALGO_93_3_16` image.
pub mod microwire {
    use super::Setup;

    pub const IMAGE: &str = "ALGO_93_3_16";

    pub fn setup(size: u32) -> Setup {
        Setup::new(super::profile(IMAGE).unwrap(), size, 0, 0)
    }

    /// Capacity from a 93xx part number ("93C46" -> 128, "93LC86" -> 2048).
    pub fn size_from_name(name: &str) -> Option<u32> {
        let base = name.split(['@', '(']).next()?.to_ascii_uppercase();
        let i = base.find("93")?;
        let rest = &base[i + 2..];
        let letters = rest.chars().take_while(|c| c.is_ascii_alphabetic()).count();
        let d: String = rest[letters..].chars().take(2).collect();
        Some(match d.as_str() {
            "06" => 32,
            "46" => 128,
            "56" => 256,
            "66" => 512,
            "76" => 1024,
            "86" => 2048,
            _ => return None,
        })
    }
}

/// Parallel NOR flash (29/39/49-series) on the `ALGO_NOR_*` images.
pub mod nor {
    use super::Setup;

    /// Command that erases the whole chip; the MCU answers with a status.
    pub const CMD_ERASE: u8 = 0x85;

    pub fn setup(size: u32, t_erase: u32, t_program: u32) -> Setup {
        Setup::new(
            super::profile("ALGO_NOR_8_1").unwrap(),
            size,
            t_erase,
            t_program,
        )
    }
}

/// Profile of a logic image, as the vendor software programs it for every
/// part using that image (captured per image; see docs/PROTOCOL.md).
pub fn profile(image: &str) -> Option<Profile> {
    let image = image.trim_end_matches(".DAT");
    PROFILES.iter().find(|(n, _)| *n == image).map(|&(_, p)| p)
}

const fn p(level: u16, ctrl: u16, vpp: u16, vcc: u16, f36: u32, f40: u32, t0: u8) -> Profile {
    Profile {
        level,
        ctrl,
        vpp,
        vcc,
        flags36: f36,
        flags40: f40,
        tail0: t0,
    }
}

static PROFILES: &[(&str, Profile)] = &[
    (
        "ALGO_27_8",
        p(0x00C0, 0x008D, 0x0294, 0x0294, 0x1, 0x8D0000, 0xEE),
    ),
    (
        "ALGO_29LV651_48_16BIT",
        p(0x00C4, 0x0212, 0x0294, 0x0294, 0x1, 0x0, 0xEE),
    ),
    (
        "ALGO_93_3_16",
        p(0x009E, 0x0212, 0x0294, 0x0294, 0x1, 0x0, 0xEE),
    ),
    (
        "ALGO_AB28F_44_16BIT",
        p(0x00A8, 0x008D, 0x0294, 0x0294, 0x1, 0x0, 0xEE),
    ),
    (
        "ALGO_AM27C2048",
        p(0x009E, 0x0085, 0x0294, 0x02C6, 0x1, 0x80, 0xEE),
    ),
    (
        "ALGO_CAT1023",
        p(0x009E, 0x0212, 0x0294, 0x0294, 0x1, 0x0, 0xEE),
    ),
    (
        "ALGO_EEPROM_1",
        p(0x009E, 0x0212, 0x0294, 0x0294, 0x1, 0x0, 0xEE),
    ),
    (
        "ALGO_FM18W08",
        p(0x009E, 0x0212, 0x0294, 0x02BC, 0x1, 0x0, 0xEE),
    ),
    (
        "ALGO_FWH_LPC",
        p(0x00C8, 0x0212, 0x0294, 0x0294, 0x82000001, 0x0, 0xEE),
    ),
    (
        "ALGO_JS28F512M_56_16BIT",
        p(0x00C4, 0x0212, 0x0294, 0x0294, 0x1, 0x0, 0x00),
    ),
    (
        "ALGO_JS28F_P33_56_16BIT",
        p(0x00DC, 0x0212, 0x0294, 0x0294, 0x1, 0x0, 0x00),
    ),
    (
        "ALGO_JS28F_P33_BGA64_16BIT",
        p(0x00C8, 0x0212, 0x0294, 0x0294, 0x1, 0x0, 0x00),
    ),
    (
        "ALGO_LH28F_1_16BIT",
        p(0x00C2, 0x008D, 0x0294, 0x0258, 0x1, 0x0, 0xEE),
    ),
    (
        "ALGO_LH28F_4_16BIT",
        p(0x00C2, 0x008D, 0x0294, 0x0258, 0x1, 0x0, 0xEE),
    ),
    (
        "ALGO_M2732A",
        p(0x009E, 0x0083, 0x0294, 0x02C6, 0x1, 0x94, 0xEE),
    ),
    (
        "ALGO_M27C1001",
        p(0x009E, 0x0083, 0x0294, 0x02C6, 0x1, 0x80, 0xEE),
    ),
    (
        "ALGO_M27C128",
        p(0x009E, 0x0088, 0x0294, 0x02C6, 0x1, 0x94, 0xEE),
    ),
    (
        "ALGO_M27C256",
        p(0x009E, 0x0085, 0x0294, 0x02C6, 0x1, 0x85, 0xEE),
    ),
    (
        "ALGO_M27C4001",
        p(0x009E, 0x0085, 0x0294, 0x02C6, 0x1, 0x85, 0xEE),
    ),
    (
        "ALGO_M27C4002",
        p(0x009E, 0x0083, 0x0294, 0x02C6, 0x1, 0x80, 0xEE),
    ),
    (
        "ALGO_M27C512",
        p(0x009E, 0x0083, 0x0294, 0x02C6, 0x1, 0x94, 0xEE),
    ),
    (
        "ALGO_M27C801",
        p(0x009E, 0x0083, 0x0294, 0x02C6, 0x1, 0x8D0080, 0xEE),
    ),
    (
        "ALGO_M28W_64_16BIT",
        p(0x00C8, 0x0212, 0x0294, 0x0294, 0x1, 0x0, 0x00),
    ),
    (
        "ALGO_N28F010",
        p(0x00A8, 0x008D, 0x0294, 0x02BC, 0x1, 0x0, 0xEE),
    ),
    (
        "ALGO_NOR_16_1",
        p(0x00C8, 0x008D, 0x0294, 0x0294, 0x80000001, 0x0, 0xEE),
    ),
    (
        "ALGO_NOR_8_1",
        p(0x00A8, 0x0212, 0x0294, 0x0258, 0x1, 0x0, 0xEE),
    ),
    (
        "ALGO_PL127J_56_16BIT",
        p(0x00C4, 0x0295, 0x0294, 0x0294, 0x1, 0x0, 0x00),
    ),
    (
        "ALGO_S29AL_64_16BIT",
        p(0x00C4, 0x008D, 0x0294, 0x0294, 0x1, 0x0, 0x00),
    ),
    (
        "ALGO_S29GL_48_8BIT",
        p(0x00C4, 0x0212, 0x0294, 0x0294, 0x1, 0x0, 0xEE),
    ),
    (
        "ALGO_S29GL_56_16BIT",
        p(0x00C8, 0x0212, 0x0294, 0x0294, 0x1, 0x0, 0x00),
    ),
    (
        "ALGO_S29GL_56_1_16BIT",
        p(0x00C8, 0x008D, 0x0294, 0x0294, 0x1, 0x0, 0x00),
    ),
    (
        "ALGO_S29GL_56_8BIT",
        p(0x00C4, 0x0212, 0x0294, 0x0294, 0x1, 0x0, 0x00),
    ),
    (
        "ALGO_S29GL_DD_56_16BIT",
        p(0x00C8, 0x0212, 0x0294, 0x0294, 0x1, 0x0, 0x00),
    ),
    (
        "ALGO_TE28F_48_16BIT",
        p(0x00C4, 0x00D3, 0x0294, 0x0294, 0x1, 0x0, 0xEE),
    ),
    (
        "ALGO_TE28F_56_16BIT",
        p(0x00C8, 0x0212, 0x0294, 0x0294, 0x1, 0x0, 0x00),
    ),
    (
        "ALGO_W27C010",
        p(0x009E, 0x008D, 0x0294, 0x02BC, 0x1, 0x8D0000, 0xEE),
    ),
    (
        "ALGO_W27C257",
        p(0x009E, 0x008D, 0x0294, 0x02BC, 0x1, 0x790000, 0xEE),
    ),
    (
        "ALGO_W27C512",
        p(0x009E, 0x008D, 0x0294, 0x02BC, 0x1, 0x8D0000, 0xEE),
    ),
    (
        "ALGO_W29EE512P",
        p(0x00A8, 0x0212, 0x0294, 0x0258, 0x1, 0x0, 0xEE),
    ),
];

/// Images whose whole read/write flow has been checked on hardware with
/// this driver. The others use rails and parameters captured from the
/// vendor software for a representative part of the same image.
pub const VERIFIED: &[&str] = &["ALGO_93_3_16"];

pub fn verified(image: &str) -> bool {
    VERIFIED.contains(&image.trim_end_matches(".DAT"))
}

/// UV/OTP EPROM images: the programming voltage differs per part and is not
/// part of the captured profile, so these parts are read-only here.
pub fn writable(image: &str) -> bool {
    !["ALGO_M27", "ALGO_AM27", "ALGO_W27", "ALGO_MX27", "ALGO_27_"]
        .iter()
        .any(|p| image.starts_with(p))
}

/// True if the image file can be found (does not touch the hardware).
pub fn image_available(name: &str) -> bool {
    find_image(name).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn microwire_block_matches_capture() {
        // 93C46 parameter block as sent by the vendor software
        let want: [u8; 40] = [
            0x80, 0, 0, 0, 0, 0, 0x9e, 0, 0x12, 2, 0x11, 2, 0x94, 2, 0x94, 2, 0, 0, 0, 0, 0, 0, 0,
            0, 8, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0x10, 0, 1, 0, 0, 0,
        ];
        let b = microwire::setup(128).params();
        assert_eq!(&b[..40], &want);
        assert!(b[40..].iter().all(|&x| x == 0));
    }

    #[test]
    fn nor_block_matches_capture() {
        // SST39SF040 parameter block as sent by the vendor software
        let want: [u8; 40] = [
            0, 0, 8, 0, 0, 0, 0xa8, 0, 0x12, 2, 0x11, 2, 0x94, 2, 0x58, 2, 0x50, 0xc3, 0, 0, 0xf4,
            1, 0, 0, 8, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0x10, 0, 1, 0, 0, 0,
        ];
        let s = nor::setup(0x80000, 50000, 500);
        assert_eq!(&s.params()[..40], &want);
        let r = s.rails();
        assert_eq!((r[1], r[15], r[18], r[19]), (0xA8, 0x294, 0x258, 0x258));
    }

    #[test]
    fn gl_block_matches_capture() {
        // EN29GL064B (8-bit bus) parameter block as sent by the vendor software
        let want: [u8; 40] = [
            0, 0, 0x80, 0, 0, 0, 0xc4, 0, 0x12, 2, 0x11, 2, 0x94, 2, 0x94, 2, 0, 0, 0, 0, 0xc8, 0,
            0, 0, 0x20, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0x10, 0, 1, 0, 0, 0,
        ];
        let mut s = Setup::new(profile("ALGO_S29GL_48_8BIT").unwrap(), 0x80_0000, 0, 200);
        s.buffer = 32;
        s.page = 0x100;
        assert_eq!(&s.params()[..40], &want);
    }

    #[test]
    fn microwire_sizes() {
        assert_eq!(microwire::size_from_name("93C46@SOIC8"), Some(128));
        assert_eq!(microwire::size_from_name("93LC86B"), Some(2048));
        assert_eq!(microwire::size_from_name("AT93C66A"), Some(512));
    }

    #[test]
    fn padding() {
        assert_eq!(blocks(&[1, 2, 3]).len(), 32768);
        assert_eq!(blocks(&vec![0; 180124]).len(), 6 * 32768);
    }
}
