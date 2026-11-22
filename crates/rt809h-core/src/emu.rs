//! Virtual chips for protocol research.
//!
//! The host software drives most non-SPI chip families by bit-banging the
//! FTDI MPSSE engine. To see a complete read/write sequence without owning
//! every chip, the bridge decodes the MPSSE command stream, replays the pin
//! activity into a software model of the chip and replaces the bytes the
//! real hardware sends back with what that chip would have answered.
//!
//! Pin use on ADBUS (fixed by the programmer's hardware):
//! bit 0 = clock (SCK / SCL), bit 1 = data out (MOSI / SDA out),
//! bit 2 = data in (MISO / SDA in), bit 3 and bit 4 = chip selects / control.

use std::collections::VecDeque;

/// A chip model: sees the pin levels the host drives and returns the level
/// it puts on the data-in line (ADBUS2).
pub trait Chip: Send {
    /// `level`/`dir` are the ADBUS output latch and direction (1 = output).
    fn step(&mut self, level: u8, dir: u8) -> bool;
    fn describe(&self) -> String;
    /// Human readable notes about decoded transactions since the last call.
    fn events(&mut self) -> Vec<String> {
        Vec::new()
    }
}

pub struct Mpsse {
    level: u8,
    dir: u8,
    /// persistent shift register for bit-mode reads
    shift: u8,
    chip: Box<dyn Chip>,
    /// bytes the chip would have answered, in order
    pub replies: VecDeque<u8>,
    din: bool,
    pub log: Vec<String>,
}

impl Mpsse {
    pub fn new(chip: Box<dyn Chip>) -> Self {
        Mpsse {
            level: 0,
            dir: 0,
            shift: 0,
            chip,
            replies: VecDeque::new(),
            din: true,
            log: Vec::new(),
        }
    }

    pub fn describe(&self) -> String {
        self.chip.describe()
    }

    pub fn events(&mut self) -> Vec<String> {
        let mut v = std::mem::take(&mut self.log);
        v.extend(self.chip.events());
        v
    }

    fn set(&mut self, level: u8, dir: u8) {
        self.level = level;
        self.dir = dir;
        self.din = self.chip.step(level, dir);
    }

    fn clk(&mut self, high: bool) {
        let l = if high {
            self.level | 1
        } else {
            self.level & !1
        };
        self.set(l, self.dir);
    }

    fn dout(&mut self, bit: bool) {
        let l = if bit { self.level | 2 } else { self.level & !2 };
        self.set(l, self.dir);
    }

    /// Clock `n` bits. `out` yields the bit to send for each clock (None =
    /// leave the data line alone). Returns the sampled data-in bits.
    fn clock(&mut self, op: u8, n: usize, out: &[bool]) -> Vec<bool> {
        let write = op & 0x10 != 0;
        let read_neg = op & 0x04 != 0;
        let mut got = Vec::with_capacity(n);
        for i in 0..n {
            if let Some(&b) = out.get(i).filter(|_| write) {
                self.dout(b);
            }
            // a chip changes its output after the edge, so sample before it
            let before_rise = self.din;
            self.clk(true);
            let before_fall = self.din;
            self.clk(false);
            got.push(if read_neg { before_fall } else { before_rise });
        }
        got
    }

    /// Feed one host write (MPSSE commands).
    pub fn feed(&mut self, p: &[u8]) {
        let mut i = 0;
        while i < p.len() {
            let op = p[i];
            i += 1;
            let arg = |k: usize| p.get(i + k).copied().unwrap_or(0);
            match op {
                0x80 => {
                    let (v, d) = (arg(0), arg(1));
                    i += 2;
                    self.set(v, d);
                }
                0x82 | 0x86 | 0x9C | 0x9D | 0x9E => i += 2,
                0x81 => {
                    let v = (self.level & !0x04) | if self.din { 0x04 } else { 0 };
                    self.replies.push_back(v);
                }
                0x83 => self.replies.push_back(0xFF),
                0x84 | 0x85 | 0x87 | 0x8A | 0x8B | 0x8C | 0x8D | 0x96 | 0x97 => {}
                0x8E => {
                    let n = arg(0) as usize + 1;
                    i += 1;
                    self.clock(0, n, &[]);
                }
                0x8F => {
                    let n = (arg(0) as usize | (arg(1) as usize) << 8) + 1;
                    i += 2;
                    self.clock(0, n * 8, &[]);
                }
                0x10..=0x3F if op & 0x40 == 0 => {
                    let bitmode = op & 0x02 != 0;
                    let lsb = op & 0x08 != 0;
                    let write = op & 0x10 != 0;
                    let read = op & 0x20 != 0;
                    if bitmode {
                        let n = arg(0) as usize + 1;
                        i += 1;
                        let data = if write {
                            i += 1;
                            p.get(i - 1).copied().unwrap_or(0)
                        } else {
                            0
                        };
                        let out: Vec<bool> = (0..n)
                            .map(|k| {
                                if lsb {
                                    data >> k & 1 != 0
                                } else {
                                    data >> (7 - k) & 1 != 0
                                }
                            })
                            .collect();
                        let got = self.clock(op, n, &out);
                        if read {
                            for b in got {
                                if lsb {
                                    self.shift = self.shift >> 1 | (b as u8) << 7;
                                } else {
                                    self.shift = self.shift << 1 | b as u8;
                                }
                            }
                            self.replies.push_back(self.shift);
                        }
                    } else {
                        let n = (arg(0) as usize | (arg(1) as usize) << 8) + 1;
                        i += 2;
                        let data: Vec<u8> = if write {
                            let d = p[i.min(p.len())..(i + n).min(p.len())].to_vec();
                            i += n;
                            d
                        } else {
                            vec![0; n]
                        };
                        for byte in data {
                            let out: Vec<bool> = (0..8)
                                .map(|k| {
                                    if lsb {
                                        byte >> k & 1 != 0
                                    } else {
                                        byte >> (7 - k) & 1 != 0
                                    }
                                })
                                .collect();
                            let got = self.clock(op, 8, &out);
                            if read {
                                let mut v = 0u8;
                                for (k, b) in got.iter().enumerate() {
                                    if *b {
                                        v |= if lsb { 1 << k } else { 0x80 >> k };
                                    }
                                }
                                self.replies.push_back(v);
                            }
                        }
                    }
                }
                _ => {
                    self.log.push(format!("emu: unhandled MPSSE op {op:02x}"));
                    return;
                }
            }
        }
    }
}

/// Fill pattern for virtual memories: recognisable and address dependent.
pub fn pattern(addr: usize) -> u8 {
    (addr as u8) ^ ((addr >> 8) as u8).wrapping_mul(0x1D) ^ 0xA5
}

/// Build a chip model from a spec like `i2c 256`.
pub fn from_spec(spec: &str) -> Option<Box<dyn Chip>> {
    let mut it = spec.split_whitespace();
    let kind = it.next()?;
    let size = parse_size(it.next().unwrap_or("256"))?;
    match kind {
        "i2c" => Some(Box::new(I2cEeprom::new(size))),
        _ => None,
    }
}

fn parse_size(s: &str) -> Option<usize> {
    let s = s.to_ascii_lowercase();
    let (n, mul) = if let Some(n) = s.strip_suffix('k') {
        (n, 1024)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 1024 * 1024)
    } else {
        (s.as_str(), 1)
    };
    n.parse::<usize>().ok().map(|v| v * mul)
}

// ---------------------------------------------------------------- I2C 24xx

#[derive(PartialEq, Clone, Copy, Debug)]
enum I2cPhase {
    Idle,
    /// receiving a byte from the host
    Rx,
    /// driving our ACK bit
    AckOut,
    /// sending a byte to the host
    Tx,
    /// waiting for the host's ACK/NACK after a byte we sent
    AckIn,
}

/// 24Cxx serial EEPROM.
pub struct I2cEeprom {
    mem: Vec<u8>,
    addr_bytes: usize,
    page: usize,
    scl: bool,
    sda: bool,
    phase: I2cPhase,
    bits: u8,
    sr: u8,
    /// bytes received in this transfer (after the device address)
    rx: Vec<u8>,
    dev: u8,
    ptr: usize,
    drive_low: bool,
    reading: bool,
    events: Vec<String>,
}

impl I2cEeprom {
    pub fn new(size: usize) -> Self {
        let (addr_bytes, page) = match size {
            0..=256 => (1, 8),
            257..=2048 => (1, 16),
            2049..=8192 => (2, 32),
            8193..=32768 => (2, 64),
            32769..=65536 => (2, 128),
            _ => (2, 256),
        };
        I2cEeprom {
            mem: (0..size).map(pattern).collect(),
            addr_bytes,
            page,
            scl: true,
            sda: true,
            phase: I2cPhase::Idle,
            bits: 0,
            sr: 0,
            rx: Vec::new(),
            dev: 0,
            ptr: 0,
            drive_low: false,
            reading: false,
            events: Vec::new(),
        }
    }

    /// Address bits carried in the device address byte (24C04..24C16, 24M01).
    fn block_bits(&self) -> u32 {
        let span = if self.addr_bytes == 1 { 256 } else { 65536 };
        (self.mem.len() / span).max(1).trailing_zeros()
    }

    fn accepts(&self, dev: u8) -> bool {
        if dev >> 4 != 0xA {
            return false;
        }
        let a = (dev >> 1) & 7;
        // pins A0..A2 tied low; bits used for memory blocks are free
        a >> self.block_bits() == 0
    }

    fn base(&self) -> usize {
        let blk = ((self.dev >> 1) & 7) as usize & ((1 << self.block_bits()) - 1);
        blk << (8 * self.addr_bytes)
    }

    fn byte_received(&mut self, b: u8) -> bool {
        if self.dev == 0 {
            if !self.accepts(b) {
                self.events.push(format!("i2c: dev {b:02x} NACK"));
                return false;
            }
            self.dev = b;
            self.reading = b & 1 != 0;
            if self.reading {
                self.events
                    .push(format!("i2c: read from {:#x}", self.ptr % self.mem.len()));
            }
            return true;
        }
        self.rx.push(b);
        if self.rx.len() == self.addr_bytes {
            let mut a = 0usize;
            for x in &self.rx {
                a = a << 8 | *x as usize;
            }
            self.ptr = (self.base() | a) % self.mem.len();
            self.events.push(format!("i2c: addr {:#x}", self.ptr));
        } else if self.rx.len() > self.addr_bytes {
            let page_base = self.ptr & !(self.page - 1);
            let n = self.rx.len() - self.addr_bytes - 1;
            let a = page_base | ((self.ptr + n) & (self.page - 1));
            let len = self.mem.len();
            self.mem[a % len] = b;
        }
        true
    }

    fn stop(&mut self) {
        if !self.reading && self.rx.len() > self.addr_bytes {
            let n = self.rx.len() - self.addr_bytes;
            self.events
                .push(format!("i2c: wrote {n} byte(s) at {:#x}", self.ptr));
        }
    }
}

impl Chip for I2cEeprom {
    fn step(&mut self, level: u8, dir: u8) -> bool {
        let scl = if dir & 1 != 0 { level & 1 != 0 } else { true };
        let host_sda = if dir & 2 != 0 { level & 2 != 0 } else { true };
        let sda = host_sda && !self.drive_low;

        if scl && self.scl && sda != self.sda {
            if !sda {
                // START / repeated START
                self.phase = I2cPhase::Rx;
                self.bits = 0;
                self.sr = 0;
                self.dev = 0;
                self.rx.clear();
                self.drive_low = false;
            } else {
                self.stop();
                self.phase = I2cPhase::Idle;
                self.drive_low = false;
            }
        } else if scl && !self.scl {
            // rising edge: sample
            match self.phase {
                I2cPhase::Rx => {
                    self.sr = self.sr << 1 | sda as u8;
                    self.bits += 1;
                }
                // NACK: host is done reading
                I2cPhase::AckIn if sda => self.phase = I2cPhase::Idle,
                _ => {}
            }
        } else if !scl && self.scl {
            // falling edge: change our output
            match self.phase {
                I2cPhase::Rx if self.bits == 8 => {
                    let ack = self.byte_received(self.sr);
                    self.drive_low = ack;
                    self.phase = if ack {
                        I2cPhase::AckOut
                    } else {
                        I2cPhase::Idle
                    };
                }
                I2cPhase::AckOut => {
                    self.bits = 0;
                    self.sr = 0;
                    if self.reading {
                        self.phase = I2cPhase::Tx;
                        self.sr = self.mem[self.ptr];
                        self.drive_low = self.sr & 0x80 == 0;
                        self.bits = 1;
                    } else {
                        self.drive_low = false;
                        self.phase = I2cPhase::Rx;
                    }
                }
                I2cPhase::Tx => {
                    if self.bits == 8 {
                        self.drive_low = false;
                        self.phase = I2cPhase::AckIn;
                        self.ptr = (self.ptr + 1) % self.mem.len();
                    } else {
                        self.drive_low = self.sr & (0x80 >> self.bits) == 0;
                        self.bits += 1;
                    }
                }
                I2cPhase::AckIn => {
                    self.sr = self.mem[self.ptr];
                    self.drive_low = self.sr & 0x80 == 0;
                    self.bits = 1;
                    self.phase = I2cPhase::Tx;
                }
                _ => {}
            }
        }
        self.scl = scl;
        self.sda = host_sda && !self.drive_low;
        self.sda
    }

    fn events(&mut self) -> Vec<String> {
        std::mem::take(&mut self.events)
    }

    fn describe(&self) -> String {
        format!("24Cxx I2C EEPROM, {} bytes", self.mem.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(m: &mut Mpsse) {
        m.feed(&[0x80, 0x03, 0x13, 0x80, 0x01, 0x13, 0x80, 0x00, 0x13]);
    }
    fn stop(m: &mut Mpsse) {
        m.feed(&[0x80, 0x00, 0x13, 0x80, 0x01, 0x13, 0x80, 0x03, 0x13]);
    }
    fn byte(m: &mut Mpsse, b: u8) -> u8 {
        m.feed(&[
            0x80, 0x00, 0x13, 0x11, 0x00, 0x00, b, 0x80, 0x02, 0x11, 0x22, 0x00, 0x87,
        ]);
        m.replies.pop_back().unwrap() & 1
    }
    fn rd(m: &mut Mpsse, last: bool) -> u8 {
        m.feed(&[0x80, 0x02, 0x11, 0x20, 0x00, 0x00]);
        let v = m.replies.pop_back().unwrap();
        let a = if last { 0x80 } else { 0x00 };
        m.feed(&[0x80, 0x00, 0x13, 0x13, 0x00, a, 0x80, 0x00, 0x13]);
        v
    }

    #[test]
    fn i2c_read_write() {
        let mut m = Mpsse::new(Box::new(I2cEeprom::new(256)));
        start(&mut m);
        assert_eq!(byte(&mut m, 0xA2), 1, "wrong address must NACK");
        stop(&mut m);
        start(&mut m);
        assert_eq!(byte(&mut m, 0xA0), 0);
        assert_eq!(byte(&mut m, 0x10), 0);
        start(&mut m);
        assert_eq!(byte(&mut m, 0xA1), 0);
        assert_eq!(rd(&mut m, false), pattern(0x10));
        assert_eq!(rd(&mut m, true), pattern(0x11));
        stop(&mut m);
        // page write then read back
        start(&mut m);
        byte(&mut m, 0xA0);
        byte(&mut m, 0x20);
        byte(&mut m, 0x12);
        byte(&mut m, 0x34);
        stop(&mut m);
        start(&mut m);
        byte(&mut m, 0xA0);
        byte(&mut m, 0x20);
        start(&mut m);
        byte(&mut m, 0xA1);
        assert_eq!(rd(&mut m, false), 0x12);
        assert_eq!(rd(&mut m, true), 0x34);
        stop(&mut m);
    }
}
