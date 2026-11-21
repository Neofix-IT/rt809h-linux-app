//! High level programmer session: connect, check contact, detect, read,
//! erase, program and verify 25-series SPI flash in the ZIF socket.

use std::time::Duration;

use crate::chips::{self, Chip};
use crate::ftdi::{self, DeviceEntry};
use crate::protocol::{ContactReport, Mcu, Voltage};
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
            return Ok(Some("No chip detected in the socket.".into()));
        }
        if r.reversed(8) {
            return Ok(Some(
                "The chip is inserted the wrong way round. Pin 1 (dot) must point to the lever, at socket pin 21.".into(),
            ));
        }
        let bad = r.check_bottom_aligned(8);
        if !bad.is_empty() {
            let list: Vec<String> = bad
                .iter()
                .map(|z| format!("chip pin {} (socket pin {z})", zif_to_pkg8(*z)))
                .collect();
            return Ok(Some(format!("Poor contact: {}.", list.join(", "))));
        }
        if r.misplaced(8) {
            return Ok(Some(
                "Something is connected outside the bottom 8 socket pins. Push the chip to the bottom of the socket.".into(),
            ));
        }
        Ok(None)
    }

    fn power_spi(&mut self, v: Voltage) -> Result<()> {
        self.mcu.set_rails(v, v)?;
        self.mcu.enter_spi(0)?;
        std::thread::sleep(Duration::from_millis(5));
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

fn zif_to_pkg8(z: u8) -> u8 {
    z - 20
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
