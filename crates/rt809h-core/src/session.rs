//! High level programmer session: connect, check contact, detect, read,
//! erase, program and verify 25-series SPI flash in the ZIF socket.

use std::time::Duration;

use crate::chips::{self, Chip};
use crate::ftdi::{self, DeviceEntry};
use crate::i2c::{Eeprom, EepromGeometry, I2cBus};
use crate::protocol::{zif_pin_8, ContactReport, Mcu, Voltage};
use crate::spi::{self, Flash, FlashGeometry, JedecId, SpiBus};
use crate::{Error, Result};

/// SCK frequency used for identification.
const ID_HZ: u32 = 1_000_000;

#[derive(Debug, Clone)]
pub struct DetectedChip {
    pub id: JedecId,
    pub chip: Option<Chip>,
    pub geometry: FlashGeometry,
    pub voltage: Voltage,
}

impl DetectedChip {
    pub fn name(&self) -> String {
        match &self.chip {
            Some(c) => c.display_name(),
            None => format!(
                "Unknown {} flash ({})",
                chips::manufacturer_name(self.id.0[0]),
                self.id
            ),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProgrammerInfo {
    pub label: String,
    pub algorithm: String,
    pub hw: Vec<u8>,
}

pub struct Programmer {
    mcu: Mcu,
    pub info: ProgrammerInfo,
    /// SCK frequency for data transfers.
    pub spi_hz: u32,
    /// routing byte that worked for I2C in this session
    i2c_mode: Option<u8>,
    log: Box<dyn FnMut(&str) + Send>,
}

fn find_device() -> Result<DeviceEntry> {
    ftdi::list_devices()?
        .into_iter()
        .next()
        .ok_or(Error::NotFound)
}

impl Programmer {
    pub fn connect() -> Result<Self> {
        Self::connect_with_log(Box::new(|m| log::info!("{m}")))
    }

    pub fn connect_with_log(mut log: Box<dyn FnMut(&str) + Send>) -> Result<Self> {
        let entry = find_device()?;
        let mut mcu = Mcu::open(&entry)?;
        let mut algorithm = mcu.algorithm()?;
        if algorithm != "BASE" {
            log(&format!("Resetting programmer logic (was {algorithm})"));
            mcu.reset_algorithm()?;
            drop(mcu);
            std::thread::sleep(Duration::from_millis(100));
            mcu = Mcu::open(&entry)?;
            algorithm = mcu.algorithm()?;
        }
        let _ = mcu.handshake()?;
        let hw = mcu.device_info()?;
        Ok(Self {
            mcu,
            info: ProgrammerInfo {
                label: entry.label(),
                algorithm,
                hw,
            },
            spi_hz: 10_000_000,
            i2c_mode: None,
            log,
        })
    }

    fn say(&mut self, m: &str) {
        (self.log)(m)
    }

    /// Raw supply monitor values.
    pub fn supply_raw(&mut self) -> Result<[u16; 3]> {
        self.mcu.measure()
    }

    /// Scan the socket with the rails set to `v` (use the lowest voltage
    /// when the chip is not known yet).
    pub fn contact_test(&mut self, v: Voltage) -> Result<ContactReport> {
        self.mcu.set_rails(v, v)?;
        let r = self.mcu.contact_test();
        let _ = self.mcu.power_off(false);
        r
    }

    /// Check an 8-pin chip sitting at the bottom of the socket. Returns a
    /// readable problem description, or `None` if all pins are fine.
    pub fn check_contact_8pin(&mut self, v: Voltage) -> Result<Option<String>> {
        let r = self.contact_test(v)?;
        log::debug!("contact scan {:02X?}", r.raw);
        if r.socket_empty() {
            return Ok(Some(
                "No chip detected. Check that a chip is inserted and the lever is closed.".into(),
            ));
        }
        let Some(p) = r.locate_8pin() else {
            return Ok(Some(format!(
                "No 8-pin chip found at the bottom of the socket (contact at socket pins {:?}).",
                r.connected_pins()
            )));
        };
        if !p.missing.is_empty() {
            let list: Vec<String> = p
                .missing
                .iter()
                .map(|&c| format!("chip pin {c} (socket pin {})", zif_pin_8(p.row, c)))
                .collect();
            let mut msg = format!("Poor contact: {}.", list.join(", "));
            if p.row != 21 {
                msg.push_str(" Push the chip to the bottom of the socket.");
            }
            return Ok(Some(msg));
        }
        if p.row != 21 {
            return Ok(Some(format!(
                "The chip sits {} row(s) too high. Push it to the bottom of the socket.",
                21 - p.row
            )));
        }
        Ok(None)
    }

    /// Power the socket at `v` and hand it to the SPI engine, following the
    /// vendor order: rails, contact scan, mode switch.
    fn power_spi(&mut self, v: Voltage) -> Result<()> {
        self.mcu.set_rails(v, v)?;
        self.mcu.contact_test()?;
        self.mcu.enter_spi()?;
        std::thread::sleep(Duration::from_millis(20));
        Ok(())
    }

    /// Switch the socket off.
    pub fn power_off(&mut self) -> Result<()> {
        self.mcu.power_off(true)
    }

    /// Read the ID several times; `None` unless all reads agree and look
    /// like a real JEDEC ID.
    fn try_id(&mut self, v: Voltage) -> Result<Option<JedecId>> {
        self.power_spi(v)?;
        let mut bus = SpiBus::init(&mut self.mcu.ftdi, ID_HZ)?;
        let _ = spi::read_signature(&mut bus)?; // wake from deep power-down
        let first = spi::read_id(&mut bus)?;
        for _ in 0..3 {
            if spi::read_id(&mut bus)? != first {
                log::debug!("unstable ID at {v} (first {first})");
                return Ok(None);
            }
        }
        Ok(first.is_valid().then_some(first))
    }

    /// An ID we can trust enough to stop ramping the voltage.
    fn plausible(id: JedecId) -> bool {
        chips::lookup(id).is_some()
            || (chips::manufacturer_name(id.0[0]) != "Unknown" && chips::size_from_id(id).is_some())
    }

    /// Identify the chip. With `voltage = None` the supply is ramped
    /// 1.8 V -> 2.5 V -> 3.3 V so that low-voltage parts are never
    /// overdriven; the chip's rated voltage is then used from the database.
    pub fn detect(&mut self, voltage: Option<Voltage>) -> Result<DetectedChip> {
        let order: Vec<Voltage> = match voltage {
            Some(v) => vec![v],
            None => Voltage::ALL.to_vec(),
        };
        let mut found = None;
        let mut last_seen = None;
        for v in order {
            self.say(&format!("Reading chip ID at {v}"));
            let id = self.try_id(v)?;
            self.mcu.power_off(true)?;
            if let Some(id) = id {
                last_seen = Some(id);
                if Self::plausible(id) {
                    found = Some((id, v));
                    break;
                }
            }
        }
        let (id, probe_v) = match (found, last_seen) {
            (Some(x), _) => x,
            (None, Some(id)) if voltage.is_some() => (id, voltage.unwrap()),
            (None, Some(id)) => {
                return Err(Error::Chip(format!(
                    "Unreliable chip ID ({id}). Check contact or select the voltage manually."
                )))
            }
            (None, None) => {
                return Err(Error::Chip(
                    "The chip did not answer (ID 00/FF). Check orientation and contact.".into(),
                ))
            }
        };
        // confirm at the rated voltage if it differs from the probe voltage
        if let (None, Some(c)) = (voltage, chips::lookup(id)) {
            if c.voltage != probe_v {
                let again = self.try_id(c.voltage)?;
                self.mcu.power_off(true)?;
                if again != Some(id) {
                    return Err(Error::Chip(format!(
                        "ID {id} found at {probe_v} could not be confirmed at {}",
                        c.voltage
                    )));
                }
            }
        }
        let chip = chips::lookup(id).copied();
        let (geometry, voltage) = match &chip {
            Some(c) => (c.geometry(), voltage.unwrap_or(c.voltage)),
            None => {
                self.power_spi(probe_v)?;
                let mut bus = SpiBus::init(&mut self.mcu.ftdi, ID_HZ)?;
                let size = spi::sfdp_size(&mut bus)?
                    .or_else(|| chips::size_from_id(id))
                    .ok_or_else(|| Error::Chip(format!("Unknown chip {id}, size unknown")))?;
                (
                    FlashGeometry {
                        size,
                        page_size: 256,
                        sector_size: 4096,
                        aai: false,
                        global_unlock: false,
                    },
                    voltage.unwrap_or(probe_v),
                )
            }
        };
        self.mcu.power_off(true)?;
        let d = DetectedChip {
            id,
            chip,
            geometry,
            voltage,
        };
        self.say(&format!(
            "Found {} ({}), {} KiB, {}",
            d.name(),
            d.id,
            d.geometry.size / 1024,
            d.voltage
        ));
        Ok(d)
    }

    /// Use a chip chosen by the user instead of auto detection. The ID is
    /// still read (at the chip's rated voltage) to make sure something
    /// answers.
    pub fn detect_as(&mut self, chip: &Chip) -> Result<DetectedChip> {
        self.say(&format!("Reading chip ID at {}", chip.voltage));
        let id = self.try_id(chip.voltage)?;
        self.mcu.power_off(true)?;
        let id = id.ok_or_else(|| {
            Error::Chip("The chip did not answer. Check orientation and contact.".into())
        })?;
        if id.as_u32() != chip.id {
            self.say(&format!(
                "Warning: chip reports {id}, expected {:06X} for {}",
                chip.id, chip.name
            ));
        }
        Ok(DetectedChip {
            id,
            chip: Some(*chip),
            geometry: chip.geometry(),
            voltage: chip.voltage,
        })
    }

    /// Power the socket for `chip` and run `f` with a ready flash handle.
    /// The socket is always switched off afterwards.
    pub fn with_flash<T>(
        &mut self,
        chip: &DetectedChip,
        f: impl FnOnce(&mut Flash) -> Result<T>,
    ) -> Result<T> {
        self.power_spi(chip.voltage)?;
        let hz = self.spi_hz;
        let res = (|| {
            let mut bus = SpiBus::init(&mut self.mcu.ftdi, ID_HZ)?;
            let _ = spi::read_signature(&mut bus)?;
            let id = spi::read_id(&mut bus)?;
            if id != chip.id {
                return Err(Error::Chip(format!(
                    "Chip ID changed ({id} instead of {}). Was the chip removed?",
                    chip.id
                )));
            }
            bus.set_speed(hz)?;
            let mut flash = Flash::new(bus, chip.geometry.clone())?;
            let r = f(&mut flash);
            let _ = flash.finish();
            r
        })();
        let off = self.mcu.power_off(true);
        let v = res?;
        off?;
        Ok(v)
    }
}

impl Programmer {
    /// Power a 24xx I2C EEPROM of `size` bytes and run `f` on it. The socket
    /// is always switched off afterwards.
    pub fn with_eeprom<T>(
        &mut self,
        size: u32,
        f: impl FnOnce(&mut Eeprom) -> Result<T>,
    ) -> Result<T> {
        // the routing byte seen in captures is 00; try the variants with the
        // connect bits set if nothing answers
        let modes: Vec<u8> = match self.i2c_mode {
            Some(m) => vec![m],
            None => vec![0x00, 0x80, 0xC0],
        };
        let geo = EepromGeometry::for_size(size);
        let mut f = Some(f);
        let mut last =
            Error::Chip("The EEPROM does not answer. Check orientation and contact.".into());
        for mode in modes {
            self.mcu.set_rails_i2c()?;
            self.mcu.enter_bitbang(mode)?;
            let res = (|| {
                let bus = I2cBus::init(&mut self.mcu.ftdi)?;
                let mut e = Eeprom::new(bus, geo);
                if !e.present()? {
                    return Ok(None);
                }
                let f = f.take().expect("called once");
                f(&mut e).map(Some)
            })();
            let off = self.mcu.power_off_bitbang(mode);
            match res {
                Ok(Some(v)) => {
                    self.i2c_mode = Some(mode);
                    off?;
                    return Ok(v);
                }
                Ok(None) => {
                    off?;
                    log::debug!("no I2C answer with routing byte {mode:02X}");
                }
                Err(e) => {
                    let _ = off;
                    last = e;
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err(last)
    }
}

impl Programmer {
    /// Make sure logic image `name` is running, uploading it if needed.
    pub fn ensure_image(&mut self, name: &str) -> Result<()> {
        if self.mcu.algorithm()?.eq_ignore_ascii_case(name) {
            return Ok(());
        }
        let path = crate::image::find_image(name)?;
        let data = std::fs::read(&path)?;
        if self.mcu.algorithm()? != "BASE" {
            self.mcu.reset_algorithm()?;
            std::thread::sleep(Duration::from_millis(100));
            self.mcu.reinit()?;
        }
        self.say(&format!("Loading programmer logic {name}"));
        self.mcu.load_image(&data)?;
        std::thread::sleep(Duration::from_millis(100));
        self.mcu.reinit()?;
        let now = self.mcu.algorithm()?;
        if !now.eq_ignore_ascii_case(name) {
            return Err(Error::Protocol(format!(
                "logic image did not start (programmer reports {now})"
            )));
        }
        self.info.algorithm = now;
        Ok(())
    }

    /// Run `f` with a chip that is handled by a logic image: the image is
    /// loaded, the rails set and the chip parameters sent first.
    pub fn with_image<T>(
        &mut self,
        image: &str,
        setup: &crate::image::Setup,
        f: impl FnOnce(&mut Mcu) -> Result<T>,
    ) -> Result<T> {
        self.ensure_image(image)?;
        let res = (|| {
            self.mcu.set_rails_words(&setup.rails())?;
            let st = self.mcu.set_params(&setup.params(), setup.tail())?;
            log::debug!("parameter status {st:02X}");
            f(&mut self.mcu)
        })();
        let fin = self.mcu.image_finish().and_then(|_| self.mcu.reinit());
        let v = res?;
        fin?;
        Ok(v)
    }

    /// Read a part that runs on a logic image.
    pub fn image_chip_read(
        &mut self,
        image: &str,
        setup: &crate::image::Setup,
        progress: crate::spi::Progress,
    ) -> Result<Vec<u8>> {
        self.with_image(image, setup, |m| {
            m.image_read(
                crate::image::CMD_READ,
                setup.tail(),
                setup.size as usize,
                progress,
            )
        })
    }

    /// Program a part that runs on a logic image (whole chip).
    pub fn image_chip_write(
        &mut self,
        image: &str,
        setup: &crate::image::Setup,
        data: &[u8],
        progress: crate::spi::Progress,
    ) -> Result<()> {
        if data.len() != setup.size as usize {
            return Err(Error::Chip(format!(
                "image is {} bytes, chip holds {}",
                data.len(),
                setup.size
            )));
        }
        self.with_image(image, setup, |m| {
            m.image_program(setup.tail(), data, progress)
        })
    }

    /// Erase a part that runs on a logic image and confirm it reads blank.
    pub fn image_chip_erase(
        &mut self,
        image: &str,
        setup: &crate::image::Setup,
        progress: crate::spi::Progress,
    ) -> Result<()> {
        let st = self.with_image(image, setup, |m| m.image_erase(setup))?;
        log::debug!("erase status {st:02X}");
        let back = self.image_chip_read(image, setup, progress)?;
        match back.iter().position(|&b| b != 0xFF) {
            None => Ok(()),
            Some(off) => Err(Error::Chip(format!("erase failed: not blank at 0x{off:X}"))),
        }
    }

    /// Read a Microwire (93xx) EEPROM.
    pub fn microwire_read(&mut self, size: u32, progress: crate::spi::Progress) -> Result<Vec<u8>> {
        use crate::image::microwire as mw;
        self.image_chip_read(mw::IMAGE, &mw::setup(size), progress)
    }

    /// Program a Microwire (93xx) EEPROM (whole chip).
    pub fn microwire_write(&mut self, data: &[u8], progress: crate::spi::Progress) -> Result<()> {
        use crate::image::microwire as mw;
        self.image_chip_write(mw::IMAGE, &mw::setup(data.len() as u32), data, progress)
    }
}

/// Compare two buffers, returning the first differing offset.
pub fn first_difference(a: &[u8], b: &[u8]) -> Option<usize> {
    a.iter()
        .zip(b.iter())
        .position(|(x, y)| x != y)
        .or(if a.len() != b.len() {
            Some(a.len().min(b.len()))
        } else {
            None
        })
}
