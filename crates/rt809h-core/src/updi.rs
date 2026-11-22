//! Microchip UPDI (tinyAVR 0/1/2, megaAVR 0, AVR DA/DB/DD) without a UART.
//!
//! UPDI is a half-duplex UART on one pin (8 data bits, even parity, two stop
//! bits). The RT809H has no UART, so the link runs on the MPSSE engine in
//! the socket's SPI routing, wired like a "SerialUPDI" adapter on the
//! contacts of an 8-pin flash: MOSI (flash pin 5) drives the UPDI pin
//! through 1 kΩ, MISO (flash pin 2) listens on it directly, GND on pin 4. The clock runs at eight times the baud rate and
//! every UART bit is one MPSSE byte (`00` or `FF`): frames are clocked out,
//! MOSI idles high, and the answer (which overrides MOSI through the
//! resistor) is sampled on MISO and decoded from the oversampled capture.
//!
//! The instruction set, ASI registers and NVM controllers follow the UPDI
//! chapters of the tinyAVR 0/1/2, megaAVR 0 and AVR DA/DB/DD datasheets.

use std::time::{Duration, Instant};

use crate::avr::{AvrPart, Bus, UpdiFamily};
use crate::ftdi::Ftdi;
use crate::spi::Progress;
use crate::{Error, Result};

/// Samples (MPSSE clocks) per UART bit.
const OS: usize = 8;
/// 12 MHz / (2 × (5 + 1)) = 1 MHz sample clock.
const DIV: u16 = 5;
pub const BAUD: u32 = 12_000_000 / (2 * (DIV as u32 + 1)) / OS as u32;

// MPSSE opcodes
const OUT_LSB: u8 = 0x19; // bytes out on -ve edge, LSB first
const IN_LSB: u8 = 0x28; // bytes in on +ve edge, LSB first
const SEND_IMMEDIATE: u8 = 0x87;
const GPIO_LOW: u8 = 0x80;
const CLK_DIV: u8 = 0x86;
const DIV5_ON: u8 = 0x8B;
const ADAPTIVE_OFF: u8 = 0x97;
const THREE_PHASE_OFF: u8 = 0x8D;
const LOOPBACK_OFF: u8 = 0x85;
/// SCK, MOSI and CS are outputs
const DIR: u8 = 0x0B;
/// MOSI high, CS low (the programmer passes MISO through only while CS
/// is low)
const IDLE: u8 = 0x02;
/// MOSI low, CS low
const BREAK: u8 = 0x00;
/// Longest capture (in UART bits) read in one USB transfer, so the reply
/// always fits the FTDI receive buffer and the clock never stalls mid-frame.
const MAX_WAVE: usize = 3500;
/// Bytes moved per block transfer.
const BLOCK: usize = 128;

// UPDI instructions
const SYNCH: u8 = 0x55;
const ACK: u8 = 0x40;
const LDS: u8 = 0x00;
const STS: u8 = 0x40;
const LD_PTR_INC: u8 = 0x24;
const ST_PTR_INC: u8 = 0x64;
const ST_PTR: u8 = 0x68;
const LDCS: u8 = 0x80;
const STCS: u8 = 0xC0;
const REPEAT: u8 = 0xA0;
const KEY: u8 = 0xE0;
const SIB_16: u8 = 0xE5;

// UPDI control/status registers
const STATUSA: u8 = 0x00;
const CTRLA: u8 = 0x02;
const CTRLB: u8 = 0x03;
const KEY_STATUS: u8 = 0x07;
const RESET_REQ: u8 = 0x08;
const SYS_STATUS: u8 = 0x0B;

/// inter-byte delay on, guard time 8 bits
const CTRLA_RUN: u8 = 0x80 | 0x04;
const CTRLA_RSD: u8 = 0x08;
const CTRLB_UPDIDIS: u8 = 0x04;
const CTRLB_CCDETDIS: u8 = 0x08;
/// guard time before the first CTRLA write is 128 bits
const GUARD_RESET: usize = 128;
const GUARD_RUN: usize = 8;

const KEY_NVMPROG: &[u8; 8] = b"NVMProg ";
const KEY_ERASE: &[u8; 8] = b"NVMErase";
const KS_ERASE: u8 = 0x08;
const KS_NVMPROG: u8 = 0x10;
const SYS_LOCK: u8 = 0x01;
const SYS_NVMPROG: u8 = 0x08;

// data space
const NVMCTRL: u32 = 0x1000;
const NVM_DATA: u32 = NVMCTRL + 6;
const NVM_ADDR: u32 = NVMCTRL + 8;
const SIGROW: u32 = 0x1100;
const EEPROM: u32 = 0x1400;

/// Fuse bytes shown and writable (WDTCFG … BOOTEND / BOOTSIZE).
pub const FUSES: usize = 9;
/// Index of SYSCFG0, which selects the function of the UPDI pin on tinyAVR.
const SYSCFG0: usize = 5;

/// NVM controller generation, from the "P:n" field of the SIB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Nvm {
    /// tinyAVR 0/1/2, megaAVR 0: page buffer, 16-bit addresses
    P0,
    /// AVR DA/DB/DD: direct word writes, 24-bit addresses
    P2,
    /// AVR EA: page buffer, 24-bit addresses
    P3,
    /// AVR DU: direct word writes like P2, registers moved
    P4,
    /// AVR EB: same programming as P3
    P5,
}

impl Nvm {
    /// NVMCTRL.STATUS (moved from +2 to +6 with P:3).
    fn status(self) -> u32 {
        match self {
            Nvm::P0 | Nvm::P2 => NVMCTRL + 2,
            _ => NVMCTRL + 6,
        }
    }

    /// Flash and EEPROM go through a page buffer (else direct writes).
    fn paged(self) -> bool {
        matches!(self, Nvm::P0 | Nvm::P3 | Nvm::P5)
    }
}

// NVMCTRL.CTRLA commands
const P0_ERWP: u8 = 0x03;
const P0_PBC: u8 = 0x04;
const P0_CHER: u8 = 0x05;
const P0_WFU: u8 = 0x07;
const P2_NOCMD: u8 = 0x00;
const P2_FLWR: u8 = 0x02;
const P2_EEERWR: u8 = 0x13;
const P2_CHER: u8 = 0x20;
// P:3 / P:5 (P:4 uses the P:2 commands)
const P3_FLPW: u8 = 0x04;
const P3_FLPBCLR: u8 = 0x0F;
const P3_EEPERW: u8 = 0x15;
const P3_EEPBCLR: u8 = 0x1F;

impl UpdiFamily {
    fn nvm(self) -> Nvm {
        match self {
            UpdiFamily::Tiny | UpdiFamily::Mega0 => Nvm::P0,
            UpdiFamily::Dx | UpdiFamily::Dd => Nvm::P2,
            UpdiFamily::Ea => Nvm::P3,
            UpdiFamily::Du => Nvm::P4,
            UpdiFamily::Eb => Nvm::P5,
        }
    }

    /// Where the flash appears in the UPDI address space.
    fn flash_base(self) -> u32 {
        match self {
            UpdiFamily::Tiny => 0x8000,
            UpdiFamily::Mega0 => 0x4000,
            _ => 0x80_0000,
        }
    }

    fn eeprom_page(self) -> usize {
        match self {
            UpdiFamily::Tiny => 32,
            UpdiFamily::Ea | UpdiFamily::Eb => 8,
            _ => 64,
        }
    }

    fn fuse_base(self) -> u32 {
        match self.nvm() {
            Nvm::P0 => 0x1280,
            _ => 0x1050,
        }
    }
}

fn family(part: &AvrPart) -> Result<UpdiFamily> {
    match part.bus {
        Bus::Updi(f) => Ok(f),
        Bus::Isp => Err(Error::Chip(format!("{} is not a UPDI part", part.name))),
    }
}

// ---------------------------------------------------------------- line codec

/// Append one 8E2 frame, one entry (`00`/`FF`) per bit.
fn push_frame(wave: &mut Vec<u8>, b: u8) {
    wave.push(0x00);
    wave.extend((0..8).map(|k| if b >> k & 1 == 1 { 0xFF } else { 0x00 }));
    wave.push(if b.count_ones() % 2 == 1 { 0xFF } else { 0x00 });
    wave.extend([0xFF, 0xFF]);
}

#[cfg(test)]
fn push_idle(wave: &mut Vec<u8>, bits: usize) {
    wave.resize(wave.len() + bits, 0xFF);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Frame {
    /// sample index of the start bit's falling edge
    at: usize,
    byte: u8,
    /// parity and stop bit correct
    ok: bool,
}

/// Find the UART frames in a capture (LSB-first samples). Bits are counted
/// from the length of each run between edges, so timing errors never add up
/// across a frame: a target whose baud rate is up to about 5 % off still
/// decodes (a UART tolerates about the same).
fn decode(raw: &[u8]) -> Vec<Frame> {
    let s: Vec<bool> = raw
        .iter()
        .flat_map(|&b| (0..8).map(move |k| b >> k & 1 == 1))
        .collect();
    let level = |i: usize| s.get(i).copied().unwrap_or(true);
    let mut out = Vec::new();
    let mut i = 1;
    while i + 12 * OS <= s.len() {
        if !(s[i - 1] && !s[i]) {
            i += 1;
            continue;
        }
        let at = i;
        let limit = at + 12 * OS;
        let mut bits = Vec::with_capacity(11);
        let (mut pos, mut v, mut rise) = (at, false, at + 1);
        while bits.len() < 11 && pos < limit {
            let mut j = pos + 1;
            while j < limit && level(j) == v {
                j += 1;
            }
            let n = ((j - pos) as f64 / OS as f64).round() as usize;
            bits.extend(std::iter::repeat_n(v, n.min(11 - bits.len())));
            if bits.is_empty() {
                break; // glitch shorter than half a bit
            }
            if !v {
                rise = j;
            }
            (pos, v) = (j, !v);
        }
        if bits.len() < 11 {
            i = at + 1;
            continue;
        }
        let byte = (0..8).fold(0u8, |acc, k| acc | (bits[1 + k] as u8) << k);
        out.push(Frame {
            at,
            byte,
            ok: bits[10] && bits[9] == (byte.count_ones() % 2 == 1),
        });
        // the next start edge comes after the last rising edge of this frame
        i = rise.max(at + 1);
    }
    out
}

/// One step of a transaction: bytes to send, then the number of bytes the
/// target answers with.
type Seg = (Vec<u8>, usize);

/// Idle bits sampled for an answer of `rx` bytes: guard time, inter-byte
/// delays and some slack.
fn listen_bits(rx: usize, guard: usize) -> usize {
    guard + rx * 16 + 16
}

/// MPSSE commands for `segs` and the number of capture bytes each segment
/// produces.
fn build(segs: &[Seg], guard: usize) -> (Vec<u8>, Vec<usize>) {
    let mut cmd = Vec::new();
    let mut reads = Vec::with_capacity(segs.len());
    for (tx, rx) in segs {
        let mut wave = Vec::with_capacity(tx.len() * 12 + 2);
        for &b in tx {
            push_frame(&mut wave, b);
        }
        cmd.extend([GPIO_LOW, IDLE, DIR]);
        for c in wave.chunks(0x10000) {
            let n = c.len() - 1;
            cmd.extend([OUT_LSB, n as u8, (n >> 8) as u8]);
            cmd.extend_from_slice(c);
        }
        cmd.extend([GPIO_LOW, IDLE, DIR]);
        let n = if *rx > 0 { listen_bits(*rx, guard) } else { 0 };
        if n > 0 {
            cmd.extend([IN_LSB, (n - 1) as u8, ((n - 1) >> 8) as u8]);
        }
        reads.push(n);
    }
    cmd.push(SEND_IMMEDIATE);
    (cmd, reads)
}

/// Pick the answers out of the captures.
fn split(raw: &[u8], segs: &[Seg], reads: &[usize]) -> Result<Vec<Vec<u8>>> {
    let mut out = Vec::with_capacity(segs.len());
    let mut at = 0;
    for ((_, rx), &n) in segs.iter().zip(reads) {
        let frames = decode(&raw[at..at + n]);
        at += n;
        if frames.len() < *rx {
            return Err(Error::Chip(format!(
                "The microcontroller does not answer on UPDI ({} of {rx} bytes). UPDI goes to \
                 the contact of flash pin 2 and through 1 kΩ to that of pin 5, GND to pin 4.",
                frames.len()
            )));
        }
        if frames.len() > *rx || frames.iter().any(|f| !f.ok) {
            return Err(Error::Protocol(format!(
                "UPDI framing or parity error ({:02X?})",
                frames.iter().map(|f| f.byte).collect::<Vec<_>>()
            )));
        }
        out.push(frames.iter().map(|f| f.byte).collect());
    }
    Ok(out)
}

// ------------------------------------------------------------------ the link

pub struct Updi<'a> {
    ftdi: &'a mut Ftdi,
    guard: usize,
    nvm: Nvm,
    sib: String,
    locked: bool,
}

impl<'a> Updi<'a> {
    /// Wake the UPDI with a double BREAK, read the System Information Block
    /// and enter NVM programming mode (unless the part is locked).
    pub fn enter(ftdi: &'a mut Ftdi) -> Result<Self> {
        let mut u = Updi {
            ftdi,
            guard: GUARD_RESET,
            nvm: Nvm::P0,
            sib: String::new(),
            locked: false,
        };
        u.ftdi.write(&[
            DIV5_ON,
            ADAPTIVE_OFF,
            THREE_PHASE_OFF,
            LOOPBACK_OFF,
            CLK_DIV,
            DIV as u8,
            (DIV >> 8) as u8,
        ])?;
        u.line(true)?;
        std::thread::sleep(Duration::from_millis(5));
        let mut last = None;
        for _ in 0..2 {
            u.double_break()?;
            match u.init() {
                Ok(()) => {
                    last = None;
                    break;
                }
                Err(e) => last = Some(e),
            }
        }
        if let Some(e) = last {
            let _ = u.line(true);
            return Err(Error::Chip(format!(
                "No UPDI device found ({e}). Wire UPDI to the contact of flash pin 2 and \
                 through 1 kΩ to that of pin 5, GND to pin 4; power the target."
            )));
        }

        let sib = u.xfer(&[(vec![SYNCH, SIB_16], 1 << 4)])?.remove(0);
        u.sib = String::from_utf8_lossy(&sib)
            .trim_end_matches('\0')
            .to_string();
        log::debug!("UPDI SIB {:?}", u.sib);
        u.nvm = match (&sib[8..10], sib[10]) {
            (b"P:", b'0') => Nvm::P0,
            (b"P:", b'2') => Nvm::P2,
            (b"P:", b'3') => Nvm::P3,
            (b"P:", b'4') => Nvm::P4,
            (b"P:", b'5') => Nvm::P5,
            (b"P:", v) => {
                return Err(Error::Chip(format!(
                    "{} uses NVM controller version {}, which is not supported yet \
                     (supported: tinyAVR 0/1/2, megaAVR 0, AVR DA/DB/DD/DU/EA/EB)",
                    u.family_name(),
                    v as char
                )))
            }
            _ => return Err(Error::Protocol(format!("unexpected UPDI SIB {:?}", u.sib))),
        };

        let sys = u.ldcs(SYS_STATUS)?;
        if sys & SYS_LOCK != 0 {
            u.locked = true;
        } else if sys & SYS_NVMPROG == 0 {
            u.enter_progmode()?;
        }
        Ok(u)
    }

    /// Device family from the SIB ("tinyAVR", "megaAVR", "AVR").
    pub fn family_name(&self) -> &str {
        self.sib.get(..7).unwrap_or(&self.sib).trim()
    }

    /// NVM controller version ("P:n" in the SIB).
    pub fn nvm_version(&self) -> char {
        match self.nvm {
            Nvm::P0 => '0',
            Nvm::P2 => '2',
            Nvm::P3 => '3',
            Nvm::P4 => '4',
            Nvm::P5 => '5',
        }
    }

    /// The part's memories are locked; only a chip erase works.
    pub fn locked(&self) -> bool {
        self.locked
    }

    /// Fail unless `part` belongs to the family the SIB reports.
    pub fn check_family(&self, part: &AvrPart) -> Result<()> {
        let fam = family(part)?;
        if fam.nvm() != self.nvm {
            return Err(Error::Chip(format!(
                "Found a {} (NVM P:{}), which is not a {}",
                self.family_name(),
                self.nvm_version(),
                part.name
            )));
        }
        Ok(())
    }

    /// Drive the line high (idle) or low (BREAK) through the resistor.
    fn line(&mut self, high: bool) -> Result<()> {
        self.ftdi
            .write(&[GPIO_LOW, if high { IDLE } else { BREAK }, DIR])
    }

    /// Two BREAKs long enough for the slowest baud rate reset the UPDI
    /// whatever state it is in (and enable it after power-up).
    fn double_break(&mut self) -> Result<()> {
        self.guard = GUARD_RESET;
        for _ in 0..2 {
            self.line(false)?;
            std::thread::sleep(Duration::from_millis(25));
            self.line(true)?;
            std::thread::sleep(Duration::from_millis(1));
        }
        std::thread::sleep(Duration::from_millis(2));
        Ok(())
    }

    fn init(&mut self) -> Result<()> {
        // the first answer comes after the reset guard time of 128 bits
        // collision detection off before the first answer: MOSI keeps
        // driving the line high through the resistor, so the target's own
        // low level never reaches a clean 0 and it would abort its reply
        self.guard = GUARD_RESET;
        self.stcs(CTRLB, CTRLB_CCDETDIS)?;
        let first = self.ldcs(STATUSA)?;
        log::debug!("UPDI STATUSA 0x{first:02X} after BREAK");
        self.stcs(CTRLA, CTRLA_RUN)?;
        self.guard = GUARD_RUN;
        let status = self.ldcs(STATUSA)?;
        if status >> 4 == 0 {
            return Err(Error::Chip(format!("UPDI STATUSA reads 0x{status:02X}")));
        }
        log::debug!("UPDI revision {}", status >> 4);
        Ok(())
    }

    /// Run one transaction; every segment's answer is returned.
    fn xfer(&mut self, segs: &[Seg]) -> Result<Vec<Vec<u8>>> {
        let (cmd, reads) = build(segs, self.guard);
        let total: usize = reads.iter().sum();
        debug_assert!(total <= MAX_WAVE, "UPDI capture of {total} bits");
        self.ftdi.write(&cmd)?;
        let raw = self.ftdi.read_exact(total)?;
        if log::log_enabled!(log::Level::Trace) {
            let low: u32 = raw.iter().map(|b| b.count_zeros()).sum();
            let lows: Vec<usize> = (0..total * 8)
                .filter(|&i| raw[i / 8] >> (i % 8) & 1 == 0)
                .take(40)
                .collect();
            log::trace!(
                "UPDI tx {:02X?}: {low} of {} samples low, at {lows:?}",
                segs.iter().map(|s| &s.0).collect::<Vec<_>>(),
                total * 8
            );
        }
        split(&raw, segs, &reads)
    }

    fn ldcs(&mut self, reg: u8) -> Result<u8> {
        Ok(self.xfer(&[(vec![SYNCH, LDCS | reg], 1)])?[0][0])
    }

    fn stcs(&mut self, reg: u8, v: u8) -> Result<()> {
        self.xfer(&[(vec![SYNCH, STCS | reg, v], 0)]).map(drop)
    }

    fn key(&mut self, key: &[u8; 8]) -> Result<()> {
        let mut tx = vec![SYNCH, KEY];
        tx.extend(key.iter().rev());
        self.xfer(&[(tx, 0)]).map(drop)
    }

    /// Size code of an address (1 = 16 bit, 2 = 24 bit) and its bytes.
    fn addr(&self, a: u32) -> (u8, Vec<u8>) {
        let b = a.to_le_bytes();
        match self.nvm {
            Nvm::P0 => (1, b[..2].to_vec()),
            _ => (2, b[..3].to_vec()),
        }
    }

    fn lds8(&mut self, a: u32) -> Result<u8> {
        let (sz, ab) = self.addr(a);
        let mut tx = vec![SYNCH, LDS | sz << 2];
        tx.extend(ab);
        Ok(self.xfer(&[(tx, 1)])?[0][0])
    }

    fn sts8(&mut self, a: u32, v: u8) -> Result<()> {
        let (sz, ab) = self.addr(a);
        let mut tx = vec![SYNCH, STS | sz << 2];
        tx.extend(ab);
        let r = self.xfer(&[(tx, 1), (vec![v], 1)])?;
        acked(&r[0])?;
        acked(&r[1])
    }

    fn set_ptr(&self, a: u32) -> Seg {
        let (sz, ab) = self.addr(a);
        let mut tx = vec![SYNCH, ST_PTR | sz];
        tx.extend(ab);
        (tx, 1)
    }

    /// Read `n` (1..=256) bytes from `a`.
    fn read_block(&mut self, a: u32, n: usize) -> Result<Vec<u8>> {
        let segs = [
            self.set_ptr(a),
            (vec![SYNCH, REPEAT, (n - 1) as u8], 0),
            (vec![SYNCH, LD_PTR_INC], n),
        ];
        let mut r = self.xfer(&segs)?;
        acked(&r[0])?;
        Ok(r.remove(2))
    }

    /// Write `data` (whole words if `words`) from `a` without per-byte
    /// acknowledges.
    fn write_block(&mut self, a: u32, data: &[u8], words: bool) -> Result<()> {
        let count = if words { data.len() / 2 } else { data.len() };
        let mut st = vec![SYNCH, ST_PTR_INC | words as u8];
        st.extend_from_slice(data);
        let segs = [
            self.set_ptr(a),
            (vec![SYNCH, STCS | CTRLA, CTRLA_RUN | CTRLA_RSD], 0),
            (vec![SYNCH, REPEAT, (count - 1) as u8], 0),
            (st, 0),
            (vec![SYNCH, STCS | CTRLA, CTRLA_RUN], 0),
        ];
        acked(&self.xfer(&segs)?[0])
    }

    fn read_mem(&mut self, a: u32, n: usize, progress: Progress) -> Result<Vec<u8>> {
        self.unlocked()?;
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            let k = BLOCK.min(n - out.len());
            out.extend(self.read_block(a + out.len() as u32, k)?);
            if !progress(out.len() as u64, n as u64) {
                return Err(Error::Cancelled);
            }
        }
        Ok(out)
    }

    fn unlocked(&self) -> Result<()> {
        if self.locked {
            return Err(Error::Chip(
                "The microcontroller is locked; erase it first (writing the flash erases it too)"
                    .into(),
            ));
        }
        Ok(())
    }

    /// System reset through the ASI.
    fn reset(&mut self) -> Result<()> {
        self.stcs(RESET_REQ, 0x59)?;
        self.stcs(RESET_REQ, 0x00)
    }

    fn wait_sys(&mut self, done: impl Fn(u8) -> bool, max: Duration, what: &str) -> Result<()> {
        let start = Instant::now();
        loop {
            if done(self.ldcs(SYS_STATUS)?) {
                return Ok(());
            }
            if start.elapsed() > max {
                return Err(Error::Timeout(what.into()));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn enter_progmode(&mut self) -> Result<()> {
        self.key(KEY_NVMPROG)?;
        if self.ldcs(KEY_STATUS)? & KS_NVMPROG == 0 {
            return Err(Error::Chip(
                "The NVM programming key was not accepted".into(),
            ));
        }
        self.reset()?;
        self.wait_sys(
            |s| s & SYS_NVMPROG != 0,
            Duration::from_millis(200),
            "the microcontroller does not enter programming mode",
        )
    }

    fn nvm_cmd(&mut self, cmd: u8) -> Result<()> {
        self.sts8(NVMCTRL, cmd)
    }

    /// Wait until the NVM controller is idle and check its error flags.
    fn wait_nvm(&mut self, max: Duration) -> Result<()> {
        let start = Instant::now();
        loop {
            let s = self.lds8(self.nvm.status())?;
            let err = match self.nvm {
                Nvm::P0 => s & 0x04,
                _ => s & 0x70,
            };
            if err != 0 {
                return Err(Error::Chip(format!("NVM write error (STATUS 0x{s:02X})")));
            }
            if s & 0x03 == 0 {
                return Ok(());
            }
            if start.elapsed() > max {
                return Err(Error::Timeout("the NVM controller stays busy".into()));
            }
            std::thread::sleep(Duration::from_micros(500));
        }
    }

    pub fn signature(&mut self) -> Result<[u8; 3]> {
        self.unlocked()?;
        let s = self.read_block(SIGROW, 3)?;
        Ok([s[0], s[1], s[2]])
    }

    /// Fuse bytes 0..9 (WDTCFG first).
    pub fn fuses(&mut self) -> Result<Vec<u8>> {
        self.unlocked()?;
        let base = match self.nvm {
            Nvm::P0 => 0x1280,
            _ => 0x1050,
        };
        self.read_block(base, FUSES)
    }

    /// Lock byte (tinyAVR/megaAVR, `C5` = unlocked) or lock key (AVR Dx,
    /// `5CC5C55C` = unlocked).
    pub fn lock_bits(&mut self) -> Result<u32> {
        self.unlocked()?;
        Ok(match self.nvm {
            Nvm::P0 => self.lds8(0x128A)? as u32,
            _ => {
                let b = self.read_block(0x1040, 4)?;
                u32::from_le_bytes([b[0], b[1], b[2], b[3]])
            }
        })
    }

    /// Program fuse `idx`. Values that would turn the UPDI pin into a GPIO
    /// or reset pin are refused: getting it back needs a 12 V pulse the
    /// RT809H cannot produce.
    pub fn write_fuse(&mut self, part: &AvrPart, idx: usize, v: u8) -> Result<()> {
        self.unlocked()?;
        let fam = family(part)?;
        if idx >= FUSES || idx == 3 || (idx == 4 && fam != UpdiFamily::Tiny) {
            return Err(Error::Chip(format!("{} has no fuse {idx}", part.name)));
        }
        if idx == SYSCFG0 {
            match fam {
                UpdiFamily::Tiny if (v >> 2) & 3 != 1 => {
                    return Err(Error::Chip(
                        "SYSCFG0 must keep RSTPINCFG = UPDI (bits 3:2 = 01), otherwise the part \
                         can only be reached with 12 V programming"
                            .into(),
                    ))
                }
                UpdiFamily::Dd | UpdiFamily::Du | UpdiFamily::Ea | UpdiFamily::Eb => {
                    return Err(Error::Chip(format!(
                        "Writing SYSCFG0 of {} is refused: on these families it can turn the \
                         UPDI pin into a GPIO",
                        part.name
                    )))
                }
                _ => {}
            }
        }
        let a = fam.fuse_base() + idx as u32;
        self.wait_nvm(Duration::from_millis(50))?;
        match self.nvm {
            Nvm::P0 => {
                self.sts8(NVM_ADDR, a as u8)?;
                self.sts8(NVM_ADDR + 1, (a >> 8) as u8)?;
                self.sts8(NVM_DATA, v)?;
                self.nvm_cmd(P0_WFU)?;
                self.wait_nvm(Duration::from_millis(50))
            }
            Nvm::P3 | Nvm::P5 => {
                self.nvm_cmd(P3_EEPBCLR)?;
                self.wait_nvm(Duration::from_millis(50))?;
                self.sts8(a, v)?;
                self.nvm_cmd(P3_EEPERW)?;
                let r = self.wait_nvm(Duration::from_millis(50));
                self.nvm_cmd(P2_NOCMD)?;
                r
            }
            Nvm::P2 | Nvm::P4 => {
                self.nvm_cmd(P2_NOCMD)?;
                self.nvm_cmd(P2_EEERWR)?;
                self.sts8(a, v)?;
                let r = self.wait_nvm(Duration::from_millis(50));
                self.nvm_cmd(P2_NOCMD)?;
                r
            }
        }
    }

    /// Erase flash, EEPROM (unless EESAVE is set) and the lock bits. A
    /// locked part is erased with the chip erase key.
    pub fn chip_erase(&mut self) -> Result<()> {
        if self.locked {
            self.key(KEY_ERASE)?;
            if self.ldcs(KEY_STATUS)? & KS_ERASE == 0 {
                return Err(Error::Chip("The chip erase key was not accepted".into()));
            }
            self.reset()?;
            self.wait_sys(
                |s| s & SYS_LOCK == 0,
                Duration::from_secs(2),
                "the chip erase does not finish",
            )?;
            self.locked = false;
            return self.enter_progmode();
        }
        self.wait_nvm(Duration::from_millis(50))?;
        match self.nvm {
            Nvm::P0 => self.nvm_cmd(P0_CHER)?,
            _ => {
                self.nvm_cmd(P2_NOCMD)?;
                self.nvm_cmd(P2_CHER)?;
            }
        }
        let r = self.wait_nvm(Duration::from_secs(2));
        if self.nvm != Nvm::P0 {
            self.nvm_cmd(P2_NOCMD)?;
        }
        r
    }

    pub fn read_flash(&mut self, part: &AvrPart, progress: Progress) -> Result<Vec<u8>> {
        let base = family(part)?.flash_base();
        self.read_mem(base, part.flash as usize, progress)
    }

    pub fn read_eeprom(&mut self, part: &AvrPart, progress: Progress) -> Result<Vec<u8>> {
        family(part)?;
        self.read_mem(EEPROM, part.eeprom as usize, progress)
    }

    /// Program flash page by page; the part must be erased. Pages that are
    /// all `FF` are skipped.
    pub fn write_flash(&mut self, part: &AvrPart, data: &[u8], progress: Progress) -> Result<()> {
        self.unlocked()?;
        let fam = family(part)?;
        let base = fam.flash_base();
        let page = part.page as usize;
        let paged = self.nvm.paged();
        if !paged {
            self.wait_nvm(Duration::from_millis(50))?;
            self.nvm_cmd(P2_NOCMD)?;
            self.nvm_cmd(P2_FLWR)?;
        }
        let r = (|| {
            for (i, chunk) in data.chunks(page).enumerate() {
                if chunk.iter().any(|&b| b != 0xFF) {
                    let mut buf = chunk.to_vec();
                    buf.resize(page, 0xFF);
                    let at = base + (i * page) as u32;
                    if paged {
                        self.wait_nvm(Duration::from_millis(50))?;
                        self.nvm_cmd(if self.nvm == Nvm::P0 {
                            P0_PBC
                        } else {
                            P3_FLPBCLR
                        })?;
                        if self.nvm != Nvm::P0 {
                            self.wait_nvm(Duration::from_millis(50))?;
                        }
                    }
                    for (j, block) in buf.chunks(BLOCK).enumerate() {
                        self.write_block(at + (j * BLOCK) as u32, block, true)?;
                        if !paged {
                            self.wait_nvm(Duration::from_millis(50))?;
                        }
                    }
                    if paged {
                        self.nvm_cmd(if self.nvm == Nvm::P0 {
                            P0_ERWP
                        } else {
                            P3_FLPW
                        })?;
                        self.wait_nvm(Duration::from_millis(50))?;
                        if self.nvm != Nvm::P0 {
                            self.nvm_cmd(P2_NOCMD)?;
                        }
                    }
                }
                if !progress(((i + 1) * page).min(data.len()) as u64, data.len() as u64) {
                    return Err(Error::Cancelled);
                }
            }
            Ok(())
        })();
        if !paged {
            self.nvm_cmd(P2_NOCMD)?;
        }
        r
    }

    /// Write the EEPROM bytes that differ from `old`.
    pub fn write_eeprom(
        &mut self,
        part: &AvrPart,
        data: &[u8],
        old: &[u8],
        progress: Progress,
    ) -> Result<()> {
        self.unlocked()?;
        let fam = family(part)?;
        match self.nvm {
            Nvm::P0 | Nvm::P3 | Nvm::P5 => {
                let p0 = self.nvm == Nvm::P0;
                let page = fam.eeprom_page();
                for (i, (new, was)) in data.chunks(page).zip(old.chunks(page)).enumerate() {
                    if new != &was[..new.len()] {
                        self.wait_nvm(Duration::from_millis(50))?;
                        self.nvm_cmd(if p0 { P0_PBC } else { P3_EEPBCLR })?;
                        if !p0 {
                            self.wait_nvm(Duration::from_millis(50))?;
                        }
                        self.write_block(EEPROM + (i * page) as u32, new, false)?;
                        self.nvm_cmd(if p0 { P0_ERWP } else { P3_EEPERW })?;
                        self.wait_nvm(Duration::from_millis(50))?;
                        if !p0 {
                            self.nvm_cmd(P2_NOCMD)?;
                        }
                    }
                    if !progress(((i + 1) * page).min(data.len()) as u64, data.len() as u64) {
                        return Err(Error::Cancelled);
                    }
                }
                Ok(())
            }
            Nvm::P2 | Nvm::P4 => {
                self.wait_nvm(Duration::from_millis(50))?;
                self.nvm_cmd(P2_NOCMD)?;
                self.nvm_cmd(P2_EEERWR)?;
                let r = (|| {
                    for (a, (&b, &o)) in data.iter().zip(old).enumerate() {
                        if b != o {
                            self.sts8(EEPROM + a as u32, b)?;
                            self.wait_nvm(Duration::from_millis(50))?;
                        }
                        if a % 32 == 31 && !progress(a as u64 + 1, data.len() as u64) {
                            return Err(Error::Cancelled);
                        }
                    }
                    progress(data.len() as u64, data.len() as u64);
                    Ok(())
                })();
                self.nvm_cmd(P2_NOCMD)?;
                r
            }
        }
    }

    /// Leave programming mode, reset the part and switch the UPDI off so the
    /// application runs.
    pub fn release(&mut self) -> Result<()> {
        let r = self
            .reset()
            .and_then(|_| self.stcs(CTRLB, CTRLB_UPDIDIS | CTRLB_CCDETDIS));
        self.line(true)?;
        r
    }
}

fn acked(r: &[u8]) -> Result<()> {
    if r.first() == Some(&ACK) {
        Ok(())
    } else {
        Err(Error::Protocol(format!("UPDI: no ACK (got {r:02X?})")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pack a bit-per-entry wave into LSB-first samples, each bit stretched
    /// by `scale` (a target running slightly off our baud rate).
    fn sample(bits: &[u8], scale: f64) -> Vec<u8> {
        let n = ((bits.len() * OS) as f64 * scale) as usize;
        let mut raw = vec![0u8; n.div_ceil(8)];
        for i in 0..n {
            let b = ((i as f64 / scale) as usize / OS).min(bits.len() - 1);
            if bits[b] != 0 {
                raw[i / 8] |= 1 << (i % 8);
            }
        }
        raw
    }

    #[test]
    fn frames_round_trip() {
        let mut w = Vec::new();
        push_idle(&mut w, 3);
        for b in [0x55, 0x00, 0xFF, 0xA5, 0x40] {
            push_frame(&mut w, b);
        }
        push_idle(&mut w, 3);
        for scale in [0.955, 0.98, 1.0, 1.02, 1.045] {
            let f = decode(&sample(&w, scale));
            let got: Vec<u8> = f.iter().map(|f| f.byte).collect();
            assert_eq!(got, [0x55, 0x00, 0xFF, 0xA5, 0x40], "scale {scale}");
            assert!(f.iter().all(|f| f.ok), "scale {scale}: {f:?}");
        }
    }

    #[test]
    fn parity_is_even() {
        let mut w = Vec::new();
        push_frame(&mut w, 0x01);
        assert_eq!(w[9], 0xFF);
        w.clear();
        push_frame(&mut w, 0x03);
        assert_eq!(w[9], 0x00);
    }

    /// The answer is found in the capture that follows its segment.
    #[test]
    fn transaction_split() {
        let segs: Vec<Seg> = vec![(vec![SYNCH, LDCS | STATUSA], 1), (vec![SYNCH, STCS, 1], 0)];
        let (_, reads) = build(&segs, GUARD_RUN);
        assert_eq!(reads[1], 0);
        let mut wave = Vec::new();
        push_idle(&mut wave, GUARD_RUN);
        push_frame(&mut wave, 0x30);
        let rest = reads[0] - wave.len();
        push_idle(&mut wave, rest);
        let r = split(&sample(&wave, 1.0), &segs, &reads).unwrap();
        assert_eq!(r, vec![vec![0x30], vec![]]);

        // no answer at all
        let idle = vec![0xFF; reads[0]];
        let e = split(&idle, &segs, &reads).unwrap_err();
        assert!(e.to_string().contains("does not answer"));
    }

    #[test]
    fn captures_fit_the_fifo() {
        let (_, reads) = build(
            &[
                (vec![SYNCH, ST_PTR | 2, 0, 0, 0x80], 1),
                (vec![SYNCH, REPEAT, (BLOCK - 1) as u8], 0),
                (vec![SYNCH, LD_PTR_INC], BLOCK),
            ],
            GUARD_RESET,
        );
        assert!(reads.iter().sum::<usize>() <= MAX_WAVE);
    }

    #[test]
    fn baud() {
        assert_eq!(BAUD, 125_000);
    }
}
