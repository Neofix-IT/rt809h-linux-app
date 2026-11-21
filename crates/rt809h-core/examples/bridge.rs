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
//! the socket disconnected. The bridge sets that bit on the way out and
//! restores the original frame in the MCU's echo.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

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

fn serve(mut s: TcpStream) -> std::io::Result<()> {
    s.set_nodelay(true)?;
    let mut chans: [Option<Ftdi>; 2] = [None, None];
    // (echo as the MCU will send it, echo the host expects)
    let mut echo_fix: Option<([u8; 10], [u8; 10])> = None;
    let t0 = Instant::now();
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
                            if let Some(fixed) = fix_spi_mode(&p) {
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
