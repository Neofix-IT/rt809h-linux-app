//! Minimal FT2232H driver on top of `nusb` (pure Rust, no libftdi/libusb).
//!
//! Only channel A is used. The programmer's microcontroller sits behind the
//! FTDI FIFO interface, and the same channel is switched into MPSSE mode for
//! fast SPI transfers once the MCU has routed the socket pins.

use std::time::{Duration, Instant};

use nusb::transfer::{Buffer, Bulk, ControlOut, ControlType, In, Out, Recipient};
use nusb::{Endpoint, Interface, MaybeFuture};

use crate::{Error, Result};

pub const VID: u16 = 0x0403;
pub const PID: u16 = 0x6010;

const REQ_RESET: u8 = 0x00;
const REQ_SET_FLOW: u8 = 0x02;
const REQ_SET_EVENT_CHAR: u8 = 0x06;
const REQ_SET_ERROR_CHAR: u8 = 0x07;
const REQ_SET_LATENCY: u8 = 0x09;
const REQ_SET_BITMODE: u8 = 0x0B;

const RESET_SIO: u16 = 0;
const PURGE_RX: u16 = 1;
const PURGE_TX: u16 = 2;

const CTRL_TIMEOUT: Duration = Duration::from_millis(1000);
const READ_CHUNK: usize = 64 * 1024;
/// Writes longer than this keep reading replies while they are sent.
const DRAIN_ABOVE: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BitMode {
    Reset = 0x00,
    Mpsse = 0x02,
}

/// Information about an attached programmer.
#[derive(Debug, Clone)]
pub struct DeviceEntry {
    pub bus: String,
    pub address: u8,
    pub serial: Option<String>,
    pub product: Option<String>,
    info: nusb::DeviceInfo,
}

impl DeviceEntry {
    pub fn label(&self) -> String {
        format!(
            "{} (bus {} addr {}, serial {})",
            self.product.as_deref().unwrap_or("RT809H"),
            self.bus,
            self.address,
            self.serial.as_deref().unwrap_or("?")
        )
    }
}

/// List FT2232H devices that look like an RT809H.
pub fn list_devices() -> Result<Vec<DeviceEntry>> {
    let devs = nusb::list_devices().wait().map_err(Error::usb)?;
    Ok(devs
        .filter(|d| d.vendor_id() == VID && d.product_id() == PID)
        .filter(|d| {
            // The RT809H EEPROM identifies itself with a "RETOOLS" manufacturer
            // string. Accept unknown strings too (some units are re-flashed).
            d.manufacturer_string()
                .map(|m| m.to_ascii_uppercase().contains("RETOOLS"))
                .unwrap_or(true)
        })
        .map(|d| DeviceEntry {
            bus: d.bus_id().to_string(),
            address: d.device_address(),
            serial: d.serial_number().map(str::to_owned),
            product: d.product_string().map(str::to_owned),
            info: d,
        })
        .collect())
}

pub struct Ftdi {
    iface: Interface,
    /// FTDI channel index used in control requests (1 = channel A, 2 = B).
    channel: u16,
    ep_out: Endpoint<Bulk, Out>,
    ep_in: Endpoint<Bulk, In>,
    max_packet: usize,
    /// Payload bytes received but not yet consumed.
    rx: Vec<u8>,
    pub read_timeout: Duration,
}

impl Ftdi {
    /// Open channel A of the programmer.
    pub fn open(entry: &DeviceEntry) -> Result<Self> {
        Self::open_channel(entry, 0)
    }

    /// Open channel `ch` (0 = A, 1 = B).
    pub fn open_channel(entry: &DeviceEntry, ch: u8) -> Result<Self> {
        let dev = entry.info.open().wait().map_err(|e| {
            Error::Usb(format!(
                "cannot open the programmer ({e}). Is the udev rule installed and the device re-plugged?"
            ))
        })?;
        let iface = dev
            .detach_and_claim_interface(ch)
            .wait()
            .map_err(|e| Error::Usb(format!("programmer is busy or not accessible ({e})")))?;
        let ep_out = iface
            .endpoint::<Bulk, Out>(0x02 + 2 * ch)
            .map_err(Error::usb)?;
        let ep_in = iface
            .endpoint::<Bulk, In>(0x81 + 2 * ch)
            .map_err(Error::usb)?;
        let max_packet = ep_in.max_packet_size();
        Ok(Self {
            iface,
            channel: ch as u16 + 1,
            ep_out,
            ep_in,
            max_packet,
            rx: Vec::new(),
            read_timeout: Duration::from_secs(3),
        })
    }

    fn control(&self, request: u8, value: u16, index: u16) -> Result<()> {
        self.iface
            .control_out(
                ControlOut {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Device,
                    request,
                    value,
                    index,
                    data: &[],
                },
                CTRL_TIMEOUT,
            )
            .wait()
            .map_err(Error::usb)
    }

    fn control_in(&self, request: u8, value: u16, index: u16, len: u16) -> Result<Vec<u8>> {
        self.iface
            .control_in(
                nusb::transfer::ControlIn {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Device,
                    request,
                    value,
                    index,
                    length: len,
                },
                CTRL_TIMEOUT,
            )
            .wait()
            .map_err(Error::usb)
    }

    /// Read the instantaneous level of the low data byte pins.
    pub fn get_bitmode(&self) -> Result<u8> {
        Ok(self
            .control_in(0x0C, 0, self.channel, 1)?
            .first()
            .copied()
            .unwrap_or(0))
    }

    /// Read one 16-bit word of the FTDI configuration EEPROM.
    pub fn read_eeprom_word(&self, addr: u16) -> Result<u16> {
        let d = self.control_in(0x90, 0, addr, 2)?;
        Ok(u16::from_le_bytes([d[0], d.get(1).copied().unwrap_or(0)]))
    }

    /// Equivalent of `FT_ResetDevice`.
    pub fn reset(&mut self) -> Result<()> {
        self.rx.clear();
        self.control(REQ_RESET, RESET_SIO, self.channel)
    }

    pub fn purge(&mut self) -> Result<()> {
        self.rx.clear();
        self.control(REQ_RESET, PURGE_RX, self.channel)?;
        self.control(REQ_RESET, PURGE_TX, self.channel)
    }

    pub fn set_bitmode(&mut self, mask: u8, mode: BitMode) -> Result<()> {
        self.set_bitmode_raw(mask, mode as u8)
    }

    pub fn set_bitmode_raw(&mut self, mask: u8, mode: u8) -> Result<()> {
        self.control(
            REQ_SET_BITMODE,
            (mask as u16) | ((mode as u16) << 8),
            self.channel,
        )
    }

    /// Bytes already buffered on the host side, after polling the device once.
    pub fn poll_available(&mut self) -> Result<usize> {
        self.fill(READ_CHUNK)?;
        Ok(self.rx.len())
    }

    /// Read up to `n` bytes, waiting at most `timeout`.
    pub fn read_timeout(&mut self, n: usize, timeout: Duration) -> Result<Vec<u8>> {
        let start = Instant::now();
        while self.rx.len() < n && start.elapsed() < timeout {
            let missing = n - self.rx.len();
            self.fill(missing)?;
        }
        let take = n.min(self.rx.len());
        let rest = self.rx.split_off(take);
        Ok(std::mem::replace(&mut self.rx, rest))
    }

    pub fn set_latency(&mut self, ms: u8) -> Result<()> {
        self.control(REQ_SET_LATENCY, ms as u16, self.channel)
    }

    pub fn disable_special_chars(&mut self) -> Result<()> {
        self.control(REQ_SET_EVENT_CHAR, 0, self.channel)?;
        self.control(REQ_SET_ERROR_CHAR, 0, self.channel)
    }

    pub fn set_flow_rts_cts(&mut self) -> Result<()> {
        self.control(REQ_SET_FLOW, 0, 0x0100 | self.channel)
    }

    pub fn write(&mut self, data: &[u8]) -> Result<()> {
        for chunk in data.chunks(READ_CHUNK) {
            let mut buf = Buffer::new(chunk.len());
            buf.extend_from_slice(chunk);
            if data.len() <= DRAIN_ABOVE {
                let c = self.ep_out.transfer_blocking(buf, Duration::from_secs(5));
                c.status.map_err(|e| Error::Usb(format!("write: {e}")))?;
                if c.actual_len != chunk.len() {
                    return Err(Error::Usb(format!(
                        "short write {}/{}",
                        c.actual_len,
                        chunk.len()
                    )));
                }
                continue;
            }
            self.ep_out.submit(buf);
            // Long MPSSE batches produce replies while they are being sent;
            // keep reading them, or the chip's FIFO fills and the write stalls.
            let start = Instant::now();
            let c = loop {
                if let Some(c) = self.ep_out.wait_next_complete(Duration::from_millis(5)) {
                    break c;
                }
                if start.elapsed() > Duration::from_secs(5) {
                    self.ep_out.cancel_all();
                    break self
                        .ep_out
                        .wait_next_complete(Duration::from_secs(1))
                        .ok_or_else(|| Error::Usb("write: no completion".into()))?;
                }
                self.fill(READ_CHUNK)?;
            };
            c.status.map_err(|e| Error::Usb(format!("write: {e}")))?;
            if c.actual_len != chunk.len() {
                return Err(Error::Usb(format!(
                    "short write {}/{}",
                    c.actual_len,
                    chunk.len()
                )));
            }
        }
        Ok(())
    }

    /// Pull one bulk transfer from the device and append payload to `rx`.
    fn fill(&mut self, want: usize) -> Result<()> {
        let pkts = want
            .div_ceil(self.max_packet - 2)
            .clamp(1, READ_CHUNK / self.max_packet);
        let len = pkts * self.max_packet;
        let c = self
            .ep_in
            .transfer_blocking(Buffer::new(len), Duration::from_millis(500));
        match c.status {
            Ok(()) => {}
            // a timed-out read still carries what arrived before the cancel
            Err(nusb::transfer::TransferError::Cancelled) => {}
            Err(e) => return Err(Error::Usb(format!("read: {e}"))),
        }
        let data = &c.buffer[..c.actual_len];
        for pkt in data.chunks(self.max_packet) {
            if pkt.len() > 2 {
                self.rx.extend_from_slice(&pkt[2..]);
            }
        }
        Ok(())
    }

    /// Read exactly `n` bytes or fail with a timeout.
    pub fn read_exact(&mut self, n: usize) -> Result<Vec<u8>> {
        let start = Instant::now();
        while self.rx.len() < n {
            if start.elapsed() > self.read_timeout {
                return Err(Error::Timeout(format!(
                    "expected {n} bytes, got {}",
                    self.rx.len()
                )));
            }
            let missing = n - self.rx.len();
            self.fill(missing)?;
        }
        let rest = self.rx.split_off(n);
        Ok(std::mem::replace(&mut self.rx, rest))
    }

    /// Read whatever arrives within `wait` (used for draining).
    pub fn read_available(&mut self, wait: Duration) -> Result<Vec<u8>> {
        let start = Instant::now();
        while start.elapsed() < wait {
            self.fill(READ_CHUNK)?;
        }
        Ok(std::mem::take(&mut self.rx))
    }
}
