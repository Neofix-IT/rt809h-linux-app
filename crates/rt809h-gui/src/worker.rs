//! Background thread that owns the programmer so the UI never blocks.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use rt809h_core::chips::Chip;
use rt809h_core::session::first_difference;
use rt809h_core::{DetectedChip, Error, Programmer, Result, Voltage};

#[derive(Clone)]
pub struct Settings {
    pub voltage: Option<Voltage>,
    pub chip: Option<Chip>,
    pub contact_check: bool,
    pub spi_mhz: u32,
}

pub enum Job {
    Connect,
    Detect(Settings),
    Contact(Settings),
    Read(Settings),
    Write(Settings, Arc<Vec<u8>>, bool),
    Verify(Settings, Arc<Vec<u8>>),
    Erase(Settings),
    Blank(Settings),
}

pub enum Event {
    Log(String),
    Progress(&'static str, f32),
    Connected(String),
    Disconnected,
    Detected(DetectedChip),
    /// Result of a contact check: `None` = all pins OK.
    Contact(Option<String>),
    Data(Vec<u8>),
    Finished(std::result::Result<String, String>),
}

pub struct Worker {
    rx: Receiver<Job>,
    tx: Sender<Event>,
    cancel: Arc<AtomicBool>,
    wake: Box<dyn Fn() + Send>,
    prog: Option<Programmer>,
}

impl Worker {
    pub fn new(
        rx: Receiver<Job>,
        tx: Sender<Event>,
        cancel: Arc<AtomicBool>,
        wake: Box<dyn Fn() + Send>,
    ) -> Self {
        Self {
            rx,
            tx,
            cancel,
            wake,
            prog: None,
        }
    }

    fn send(&self, e: Event) {
        let _ = self.tx.send(e);
        (self.wake)();
    }

    pub fn run(mut self) {
        while let Ok(job) = self.rx.recv() {
            self.cancel.store(false, Ordering::SeqCst);
            let res = self.handle(job);
            if let Err(e) = &res {
                if matches!(
                    e,
                    Error::Usb(_) | Error::Timeout(_) | Error::NotFound | Error::Protocol(_)
                ) {
                    self.prog = None;
                    self.send(Event::Disconnected);
                }
            }
            self.send(Event::Finished(res.map_err(|e| e.to_string())));
        }
    }

    fn programmer(&mut self) -> Result<&mut Programmer> {
        if self.prog.is_none() {
            let tx = self.tx.clone();
            let p = Programmer::connect_with_log(Box::new(move |m| {
                let _ = tx.send(Event::Log(m.to_string()));
            }))?;
            self.send(Event::Connected(p.info.label.clone()));
            self.prog = Some(p);
        }
        Ok(self.prog.as_mut().unwrap())
    }

    fn prepare(&mut self, s: &Settings) -> Result<DetectedChip> {
        let tx = self.tx.clone();
        let p = self.programmer()?;
        p.spi_hz = s.spi_mhz.clamp(1, 30) * 1_000_000;
        if s.contact_check {
            let v = s
                .chip
                .map(|c| c.voltage)
                .or(s.voltage)
                .unwrap_or(Voltage::V1_8);
            let problem = p.check_contact_8pin(v)?;
            let _ = tx.send(Event::Contact(problem.clone()));
            if let Some(problem) = problem {
                return Err(Error::Chip(problem));
            }
            let _ = tx.send(Event::Log("Contact check OK".into()));
        }
        let chip = match &s.chip {
            Some(c) => p.detect_as(c)?,
            None => p.detect(s.voltage)?,
        };
        self.send(Event::Detected(chip.clone()));
        Ok(chip)
    }

    fn progress_fn(&self, label: &'static str) -> impl FnMut(u64, u64) -> bool {
        let tx = self.tx.clone();
        let cancel = self.cancel.clone();
        let mut last = -1i32;
        move |done, total| {
            let f = if total == 0 {
                0.0
            } else {
                done as f32 / total as f32
            };
            let pct = (f * 200.0) as i32;
            if pct != last {
                last = pct;
                let _ = tx.send(Event::Progress(label, f));
            }
            !cancel.load(Ordering::SeqCst)
        }
    }

    fn handle(&mut self, job: Job) -> Result<String> {
        match job {
            Job::Connect => {
                self.prog = None;
                let p = self.programmer()?;
                Ok(format!("Connected: {}", p.info.label))
            }
            Job::Contact(s) => {
                let p = self.programmer()?;
                let v = s
                    .chip
                    .map(|c| c.voltage)
                    .or(s.voltage)
                    .unwrap_or(Voltage::V1_8);
                let problem = p.check_contact_8pin(v)?;
                self.send(Event::Contact(problem.clone()));
                match problem {
                    Some(problem) => Err(Error::Chip(problem)),
                    None => Ok("All 8 pins have good contact".into()),
                }
            }
            Job::Detect(s) => {
                let c = self.prepare(&s)?;
                Ok(format!("Detected {}", c.name()))
            }
            Job::Read(s) => {
                let c = self.prepare(&s)?;
                let mut pr = self.progress_fn("Reading");
                let size = c.geometry.size;
                let data = self
                    .prog
                    .as_mut()
                    .unwrap()
                    .with_flash(&c, |f| f.read(0, size, &mut pr))?;
                self.send(Event::Data(data));
                Ok(format!("Read {} KiB", size / 1024))
            }
            Job::Write(s, data, verify) => {
                let c = self.prepare(&s)?;
                if data.len() as u64 > c.geometry.size as u64 {
                    return Err(Error::Chip(format!(
                        "File ({} bytes) is larger than the chip ({} bytes)",
                        data.len(),
                        c.geometry.size
                    )));
                }
                let mut pe = self.progress_fn("Erasing");
                let mut pw = self.progress_fn("Writing");
                let mut pv = self.progress_fn("Verifying");
                let tx = self.tx.clone();
                self.prog.as_mut().unwrap().with_flash(&c, |f| {
                    let _ = tx.send(Event::Log("Erasing".into()));
                    if data.len() as u32 == c.geometry.size {
                        f.erase_chip(&mut pe)?;
                    } else {
                        f.erase_range(0, data.len() as u32, &mut pe)?;
                    }
                    let _ = tx.send(Event::Log("Programming".into()));
                    f.program(0, &data, &mut pw)?;
                    if verify {
                        let _ = tx.send(Event::Log("Verifying".into()));
                        let back = f.read(0, data.len() as u32, &mut pv)?;
                        if let Some(off) = first_difference(&data, &back) {
                            return Err(Error::Chip(format!("Verify failed at 0x{off:X}")));
                        }
                    }
                    Ok(())
                })?;
                Ok(if verify {
                    "Write and verify OK".into()
                } else {
                    "Write OK".into()
                })
            }
            Job::Verify(s, data) => {
                let c = self.prepare(&s)?;
                let len = (data.len() as u32).min(c.geometry.size);
                let mut pv = self.progress_fn("Verifying");
                let back = self
                    .prog
                    .as_mut()
                    .unwrap()
                    .with_flash(&c, |f| f.read(0, len, &mut pv))?;
                match first_difference(&data[..len as usize], &back) {
                    None => Ok("Verify OK".into()),
                    Some(off) => Err(Error::Chip(format!("Mismatch at 0x{off:X}"))),
                }
            }
            Job::Erase(s) => {
                let c = self.prepare(&s)?;
                let mut pe = self.progress_fn("Erasing");
                self.prog
                    .as_mut()
                    .unwrap()
                    .with_flash(&c, |f| f.erase_chip(&mut pe))?;
                Ok("Chip erased".into())
            }
            Job::Blank(s) => {
                let c = self.prepare(&s)?;
                let mut pr = self.progress_fn("Checking");
                let size = c.geometry.size;
                let data = self
                    .prog
                    .as_mut()
                    .unwrap()
                    .with_flash(&c, |f| f.read(0, size, &mut pr))?;
                match data.iter().position(|&b| b != 0xFF) {
                    None => Ok("Chip is blank".into()),
                    Some(off) => Err(Error::Chip(format!("Not blank (first data at 0x{off:X})"))),
                }
            }
        }
    }
}
