//! Protocol research tool: exposes the programmer's FTDI channels over TCP so
//! that a stand-in `ftd2xx.dll` (see tools/ftd2xx-bridge) can forward calls
//! from another host application. Every call is logged with its data, which
//! is how the MCU command set was documented.
//!
//! Wire format (little endian): request = op:u8 ch:u8 len:u32 payload,
//! response = status:u32 len:u32 payload.
//!
//! Compatibility fix: some host software sends the SPI socket command (`52`)
//! and the matching power-off (`01`) with mode byte bit 7 clear, which leaves
//! the socket disconnected. The same `52 00 00` frame is correct for the ISP
//! header, so a `52` is forwarded as sent and only re-sent with bit 7 set
//! when the host goes on to an SPI session (MPSSE on all pins) instead of
//! reopening the channel for ISP. Power-off frames are fixed on the way out
//! and the original is restored in the MCU's echo.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use rt809h_core::emu;
use rt809h_core::ftdi::{self, Ftdi};
use rt809h_core::protocol::frame;

/// Set bit 7 of the mode byte in SPI `52`/`01` frames. Returns the frame to
/// send and, if it was changed, the original.
fn fix_spi_mode(p: &[u8]) -> Option<[u8; 10]> {
    if p.len() != 10 || p[0] != 0xAA || !matches!(p[1], 0x52 | 0x01) {
        return None;
    }
    if p[5..] != [1, 1, 1, 1, 1] || p[3] & 0x80 != 0 {
        return None;
    }
    Some(frame(p[1], p[2], p[3] | 0x80, [1, 1, 1, 1, 1]))
}

/// Send a routing frame on our own (MCU sync pulse first) and drop its echo.
fn resend_route(f: &mut Ftdi, frame: &[u8; 10]) -> rt809h_core::Result<()> {
    f.set_bitmode_raw(0xFF, 0x02)?;
    f.write(&[0x82, 0x00, 0x40])?;
    std::thread::sleep(Duration::from_millis(3));
    f.write(&[0x82, 0xFF, 0x00])?;
    f.set_bitmode_raw(0xFF, 0x00)?;
    std::thread::sleep(Duration::from_millis(50));
    f.write(frame)?;
    f.read_timeout(10, Duration::from_millis(500))?;
    Ok(())
}

fn hex(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn u32le(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

fn main() {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "0.0.0.0:9809".into());
    let listener = TcpListener::bind(&addr).unwrap();
    eprintln!("bridge listening on {addr}");
    for s in listener.incoming() {
        let s = s.unwrap();
        eprintln!("client connected");
        if let Err(e) = serve(s) {
            eprintln!("client ended: {e}");
        }
    }
}

fn load_vchip() -> Option<emu::Mpsse> {
    let path = std::env::var("RT809H_EMU").ok()?;
    let spec = std::fs::read_to_string(path).ok()?;
    emu::from_spec(spec.trim()).map(emu::Mpsse::new)
}

fn serve(mut s: TcpStream) -> std::io::Result<()> {
    s.set_nodelay(true)?;
    let mut chans: [Option<Ftdi>; 2] = [None, None];
    // (echo as the MCU will send it, echo the host expects)
    let mut echo_fix: Option<([u8; 10], [u8; 10])> = None;
    let t0 = Instant::now();
    // virtual chip (see emu.rs), configured by the file named in RT809H_EMU
    let mut vchip: Option<emu::Mpsse> = None;
    let mut bitmode: u8 = 0;
    // corrected `52` frame to send if an SPI session follows
    let mut pending_spi: Option<[u8; 10]> = None;
    loop {
        let mut hdr = [0u8; 6];
        s.read_exact(&mut hdr)?;
        let (op, ch) = (hdr[0], (hdr[1] & 1) as usize);
        let len = u32le(&hdr[2..]) as usize;
        let mut p = vec![0u8; len];
        s.read_exact(&mut p)?;
        let ts = t0.elapsed().as_secs_f64();
        let mut out: Vec<u8> = Vec::new();
        let mut status: u32 = 0;
        let res: rt809h_core::Result<()> = (|| {
            match op {
                1 => {
                    let devs = ftdi::list_devices()?;
                    out.push(devs.len() as u8);
                    if let Some(d) = devs.first() {
                        let sn = d.serial.clone().unwrap_or_default();
                        let pr = d.product.clone().unwrap_or_default();
                        out.extend_from_slice(sn.as_bytes());
                        out.push(0);
                        out.extend_from_slice(pr.as_bytes());
                        out.push(0);
                    }
                }
                2 => {
                    let devs = ftdi::list_devices()?;
                    let d = devs.first().ok_or(rt809h_core::Error::NotFound)?;
                    chans[ch] = None;
                    chans[ch] = Some(Ftdi::open_channel(d, ch as u8)?);
                    println!("{ts:9.4} [{ch}] OPEN");
                }
                3 => {
                    pending_spi = None;
                    chans[ch] = None;
                    println!("{ts:9.4} [{ch}] CLOSE");
                }
                0x10 => {
                    // log-only message from the shim
                    println!("{ts:9.4} [{ch}] NOTE {}", String::from_utf8_lossy(&p));
                }
                _ => {
                    let f = chans[ch]
                        .as_mut()
                        .ok_or_else(|| rt809h_core::Error::Usb("not open".into()))?;
                    match op {
                        4 => {
                            pending_spi = None;
                            f.reset()?;
                            println!("{ts:9.4} [{ch}] RESET_DEVICE");
                        }
                        5 => {
                            let m = u32le(&p);
                            if m & 1 != 0 {
                                f.purge()?;
                            }
                            println!("{ts:9.4} [{ch}] PURGE mask={m}");
                        }
                        6 => {
                            if let Some(fixed) = pending_spi.take().filter(|_| p == [0xFF, 0x02]) {
                                resend_route(f, &fixed)?;
                                println!("{ts:9.4} [{ch}] FIX SPI route -> {}", hex(&fixed));
                            }
                            bitmode = p[1];
                            f.set_bitmode_raw(p[0], p[1])?;
                            println!(
                                "{ts:9.4} [{ch}] SETBITMODE mask=0x{:02x} mode=0x{:02x}",
                                p[0], p[1]
                            );
                        }
                        7 => {
                            f.set_latency(p[0])?;
                            println!("{ts:9.4} [{ch}] LATENCY {}", p[0]);
                        }
                        8 => {
                            if bitmode == 0 && p.len() == 10 && p[0] == 0xAA {
                                match p[1] {
                                    0x52 => {
                                        vchip = load_vchip();
                                        if let Some(v) = &vchip {
                                            println!("{ts:9.4} [{ch}] EMU {}", v.describe());
                                        }
                                    }
                                    0x01 => vchip = None,
                                    _ => {}
                                }
                            } else if bitmode == 2 {
                                if let Some(v) = vchip.as_mut() {
                                    v.feed(&p);
                                    for e in v.events() {
                                        println!("{ts:9.4} [{ch}] EMU {e}");
                                    }
                                }
                            }
                            pending_spi = None;
                            let fixed = fix_spi_mode(&p);
                            if let (Some(x), 0x52) = (fixed, p.get(1).copied().unwrap_or(0)) {
                                pending_spi = Some(x);
                                f.write(&p)?;
                            } else if let Some(fixed) = fixed {
                                let mut want: [u8; 10] = p[..].try_into().unwrap();
                                want[0] = 0x55;
                                let mut got = fixed;
                                got[0] = 0x55;
                                echo_fix = Some((got, want));
                                println!("{ts:9.4} [{ch}] FIX mode byte -> {}", hex(&fixed));
                                f.write(&fixed)?;
                            } else {
                                f.write(&p)?;
                            }
                            out.extend_from_slice(&(p.len() as u32).to_le_bytes());
                            println!("{ts:9.4} [{ch}] WRITE {}: {}", p.len(), hex(&p));
                        }
                        9 => {
                            let n = u32le(&p) as usize;
                            let to = u32le(&p[4..]);
                            let mut d =
                                f.read_timeout(n, Duration::from_millis(to.max(1) as u64))?;
                            if let Some(v) = vchip.as_mut().filter(|_| bitmode == 2) {
                                for b in d.iter_mut() {
                                    match v.replies.pop_front() {
                                        Some(x) => *b = x,
                                        None => break,
                                    }
                                }
                            }
                            if let Some((got, want)) = echo_fix {
                                if d.len() >= 10 && d[..10] == got {
                                    d[..10].copy_from_slice(&want);
                                    echo_fix = None;
                                }
                            }
                            println!("{ts:9.4} [{ch}] READ {}/{}: {}", d.len(), n, hex(&d));
                            out = d;
                        }
                        10 => {
                            let n = f.poll_available()?;
                            out.extend_from_slice(&(n as u32).to_le_bytes());
                        }
                        11 => {
                            let v = f.get_bitmode()?;
                            out.push(v);
                            println!("{ts:9.4} [{ch}] GETBITMODE -> 0x{v:02x}");
                        }
                        12 => {
                            let a = u32le(&p) as u16;
                            let v = f.read_eeprom_word(a)?;
                            out.extend_from_slice(&v.to_le_bytes());
                            println!("{ts:9.4} [{ch}] READEE {a} -> 0x{v:04x}");
                        }
                        _ => {
                            println!("{ts:9.4} [{ch}] unknown op {op}");
                        }
                    }
                }
            }
            Ok(())
        })();
        if let Err(e) = res {
            println!("{ts:9.4} [{ch}] op {op} ERROR {e}");
            status = 4; // FT_IO_ERROR
            out.clear();
        }
        let mut resp = Vec::with_capacity(8 + out.len());
        resp.extend_from_slice(&status.to_le_bytes());
        resp.extend_from_slice(&(out.len() as u32).to_le_bytes());
        resp.extend_from_slice(&out);
        s.write_all(&resp)?;
    }
}
