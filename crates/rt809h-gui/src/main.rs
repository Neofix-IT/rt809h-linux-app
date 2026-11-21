#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod worker;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

use eframe::egui::{self, Color32, RichText};
use rt809h_core::chips::{self, Chip};
use rt809h_core::{DetectedChip, Voltage};
use worker::{Event, Job, Settings, Worker};

const APP_ID: &str = "rt809h-linux";

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RT809H Programmer")
            .with_app_id(APP_ID)
            .with_inner_size([860.0, 640.0])
            .with_min_inner_size([640.0, 480.0])
            .with_icon(icon()),
        ..Default::default()
    };
    eframe::run_native(
        "RT809H Programmer",
        options,
        Box::new(|cc| Ok(Box::new(App::new(&cc.egui_ctx)))),
    )
}

#[derive(PartialEq, Clone, Copy)]
enum VoltChoice {
    Auto,
    Fixed(Voltage),
}

struct Buffer {
    data: Arc<Vec<u8>>,
    origin: String,
}

struct App {
    jobs: Sender<Job>,
    events: Receiver<Event>,
    cancel: Arc<AtomicBool>,
    busy: bool,
    connected: Option<String>,
    detected: Option<DetectedChip>,
    volt: VoltChoice,
    chip: Option<Chip>,
    chip_filter: String,
    contact_check: bool,
    verify_after_write: bool,
    spi_mhz: u32,
    buffer: Option<Buffer>,
    progress: Option<(&'static str, f32)>,
    status: Option<(bool, String)>,
    log: Vec<String>,
}

impl App {
    fn new(ctx: &egui::Context) -> Self {
        let (jtx, jrx) = channel();
        let (etx, erx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let ctx2 = ctx.clone();
        let w = Worker::new(
            jrx,
            etx,
            cancel.clone(),
            Box::new(move || ctx2.request_repaint()),
        );
        std::thread::spawn(move || w.run());
        let mut app = Self {
            jobs: jtx,
            events: erx,
            cancel,
            busy: false,
            connected: None,
            detected: None,
            volt: VoltChoice::Auto,
            chip: None,
            chip_filter: String::new(),
            contact_check: true,
            verify_after_write: true,
            spi_mhz: 10,
            buffer: None,
            progress: None,
            status: None,
            log: Vec::new(),
        };
        app.start(Job::Connect);
        app
    }

    fn settings(&self) -> Settings {
        Settings {
            voltage: match self.volt {
                VoltChoice::Auto => None,
                VoltChoice::Fixed(v) => Some(v),
            },
            chip: self.chip,
            contact_check: self.contact_check,
            spi_mhz: self.spi_mhz,
        }
    }

    fn start(&mut self, job: Job) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.progress = None;
        self.status = None;
        let _ = self.jobs.send(job);
    }

    fn poll(&mut self) {
        while let Ok(ev) = self.events.try_recv() {
            match ev {
                Event::Log(m) => self.log.push(m),
                Event::Progress(l, f) => self.progress = Some((l, f)),
                Event::Connected(l) => self.connected = Some(l),
                Event::Disconnected => self.connected = None,
                Event::Detected(c) => self.detected = Some(c),
                Event::Data(d) => {
                    let origin = match &self.detected {
                        Some(c) => format!("read from {}", c.name()),
                        None => "read from chip".into(),
                    };
                    self.buffer = Some(Buffer {
                        data: Arc::new(d),
                        origin,
                    });
                }
                Event::Finished(r) => {
                    self.busy = false;
                    self.progress = None;
                    let (ok, msg) = match r {
                        Ok(m) => (true, m),
                        Err(m) => (false, m),
                    };
                    self.log.push(if ok {
                        msg.clone()
                    } else {
                        format!("Error: {msg}")
                    });
                    self.status = Some((ok, msg));
                }
            }
        }
        if self.log.len() > 500 {
            self.log.drain(..self.log.len() - 500);
        }
    }

    fn open_file(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Binary", &["bin", "rom", "img", "BIN"])
            .add_filter("All files", &["*"])
            .pick_file()
        {
            match std::fs::read(&path) {
                Ok(d) => {
                    self.log
                        .push(format!("Loaded {} ({} bytes)", path.display(), d.len()));
                    self.buffer = Some(Buffer {
                        data: Arc::new(d),
                        origin: name_of(&path),
                    });
                }
                Err(e) => self.status = Some((false, format!("Cannot open file: {e}"))),
            }
        }
    }

    fn save_file(&mut self) {
        let Some(buf) = &self.buffer else { return };
        let default = match &self.detected {
            Some(c) => format!("{}.bin", c.name().replace([' ', '/'], "_")),
            None => "dump.bin".into(),
        };
        if let Some(path) = rfd::FileDialog::new().set_file_name(default).save_file() {
            match std::fs::write(&path, buf.data.as_slice()) {
                Ok(()) => self.log.push(format!("Saved {}", path.display())),
                Err(e) => self.status = Some((false, format!("Cannot save file: {e}"))),
            }
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let (dot, text) = match &self.connected {
                Some(l) => (Color32::from_rgb(60, 180, 90), format!("Connected: {l}")),
                None => (
                    Color32::from_rgb(200, 70, 60),
                    "Programmer not connected".into(),
                ),
            };
            let (r, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
            ui.painter().circle_filled(r.center(), 5.0, dot);
            ui.label(text);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(!self.busy, egui::Button::new("Reconnect"))
                    .clicked()
                {
                    self.start(Job::Connect);
                }
            });
        });
    }

    fn chip_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Chip");
        ui.add_space(4.0);
        egui::Grid::new("chip_grid")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label("Type");
                let label = match &self.chip {
                    Some(c) => c.display_name(),
                    None => "Auto detect (25xx SPI flash)".into(),
                };
                egui::ComboBox::from_id_salt("chip_sel")
                    .width(260.0)
                    .selected_text(label)
                    .show_ui(ui, |ui| {
                        ui.text_edit_singleline(&mut self.chip_filter)
                            .on_hover_text("Filter");
                        if ui
                            .selectable_label(self.chip.is_none(), "Auto detect")
                            .clicked()
                        {
                            self.chip = None;
                        }
                        for c in chips::search(&self.chip_filter).into_iter().take(200) {
                            let sel = self.chip.map(|x| x.id == c.id).unwrap_or(false);
                            let text = format!(
                                "{}  ({}, {})",
                                c.display_name(),
                                size_str(c.size),
                                c.voltage
                            );
                            if ui.selectable_label(sel, text).clicked() {
                                self.chip = Some(*c);
                            }
                        }
                    });
                ui.end_row();

                ui.label("Voltage");
                ui.add_enabled_ui(self.chip.is_none(), |ui| {
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.volt, VoltChoice::Auto, "Auto");
                        for v in Voltage::ALL {
                            ui.selectable_value(&mut self.volt, VoltChoice::Fixed(v), v.label());
                        }
                    });
                });
                ui.end_row();

                ui.label("Detected");
                match &self.detected {
                    Some(c) => {
                        ui.label(
                            RichText::new(format!(
                                "{}   ID {}   {}   {}",
                                c.name(),
                                c.id,
                                size_str(c.geometry.size),
                                c.voltage
                            ))
                            .strong(),
                        );
                    }
                    None => {
                        ui.label(RichText::new("–").weak());
                    }
                }
                ui.end_row();
            });

        ui.add_space(6.0);
        egui::CollapsingHeader::new("Options").show(ui, |ui| {
            ui.checkbox(
                &mut self.contact_check,
                "Check pin contact before every operation",
            );
            ui.checkbox(&mut self.verify_after_write, "Verify after writing");
            ui.horizontal(|ui| {
                ui.label("SPI clock");
                ui.add(egui::Slider::new(&mut self.spi_mhz, 1..=30).suffix(" MHz"));
            });
        });
    }

    fn actions(&mut self, ui: &mut egui::Ui) {
        let s = self.settings();
        let has_buf = self.buffer.is_some();
        let idle = !self.busy;
        let big = |t: &str| {
            egui::Button::new(RichText::new(t).size(16.0)).min_size(egui::vec2(110.0, 34.0))
        };
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(idle, big("Detect"))
                .on_hover_text("Check contact and identify the chip")
                .clicked()
            {
                self.start(Job::Detect(s.clone()));
            }
            if ui
                .add_enabled(idle, big("Read"))
                .on_hover_text("Read the chip into the buffer")
                .clicked()
            {
                self.start(Job::Read(s.clone()));
            }
            if ui
                .add_enabled(idle && has_buf, big("Write"))
                .on_hover_text("Erase, program and verify the buffer")
                .clicked()
            {
                let d = self.buffer.as_ref().unwrap().data.clone();
                self.start(Job::Write(s.clone(), d, self.verify_after_write));
            }
            if ui
                .add_enabled(idle && has_buf, big("Verify"))
                .on_hover_text("Compare chip with the buffer")
                .clicked()
            {
                let d = self.buffer.as_ref().unwrap().data.clone();
                self.start(Job::Verify(s.clone(), d));
            }
            if ui.add_enabled(idle, big("Erase")).clicked() {
                self.start(Job::Erase(s.clone()));
            }
            if ui.add_enabled(idle, big("Blank check")).clicked() {
                self.start(Job::Blank(s.clone()));
            }
            if ui.add_enabled(idle, big("Contact test")).clicked() {
                self.start(Job::Contact(s.clone()));
            }
        });
    }

    fn progress_row(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if self.busy {
                let (label, f) = self.progress.unwrap_or(("Working", 0.0));
                let w = ui.available_width() - 90.0;
                ui.add(
                    egui::ProgressBar::new(f)
                        .desired_width(w.max(100.0))
                        .text(format!("{label} {:.0}%", f * 100.0))
                        .animate(true),
                );
                if ui.button("Cancel").clicked() {
                    self.cancel.store(true, Ordering::SeqCst);
                }
            } else if let Some((ok, msg)) = &self.status {
                let color = if *ok {
                    Color32::from_rgb(60, 170, 90)
                } else {
                    Color32::from_rgb(215, 75, 60)
                };
                let (r, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                ui.painter().circle_filled(r.center(), 6.0, color);
                ui.label(RichText::new(msg).color(color).strong());
            } else {
                ui.label(RichText::new("Ready").weak());
            }
        });
    }

    fn buffer_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Buffer");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(self.buffer.is_some(), egui::Button::new("Save…"))
                    .clicked()
                {
                    self.save_file();
                }
                if ui
                    .add_enabled(!self.busy, egui::Button::new("Open file…"))
                    .clicked()
                {
                    self.open_file();
                }
            });
        });
        match &self.buffer {
            None => {
                ui.label(RichText::new("Empty – open a file or read a chip.").weak());
            }
            Some(b) => {
                ui.label(format!(
                    "{}  ·  {} bytes  ·  CRC32 {:08X}",
                    b.origin,
                    b.data.len(),
                    crc32(&b.data)
                ));
                let data = b.data.clone();
                let rows = data.len().div_ceil(16);
                let h = ui.text_style_height(&egui::TextStyle::Monospace);
                egui::ScrollArea::vertical()
                    .id_salt("hex")
                    .max_height(ui.available_height() - 140.0)
                    .auto_shrink([false, false])
                    .show_rows(ui, h, rows, |ui, range| {
                        for r in range {
                            let off = r * 16;
                            let line = &data[off..(off + 16).min(data.len())];
                            let hex: String = line.iter().map(|b| format!("{b:02X} ")).collect();
                            let asc: String = line
                                .iter()
                                .map(|&b| {
                                    if (0x20..0x7F).contains(&b) {
                                        b as char
                                    } else {
                                        '.'
                                    }
                                })
                                .collect();
                            ui.label(
                                RichText::new(format!("{off:08X}  {hex:<48} {asc}")).monospace(),
                            );
                        }
                    });
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll();
        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(4.0);
            self.top_bar(ui);
            ui.add_space(2.0);
        });
        egui::Panel::bottom("log")
            .resizable(true)
            .default_size(120.0)
            .show(ui, |ui| {
                ui.add_space(4.0);
                ui.label(RichText::new("Log").strong());
                egui::ScrollArea::vertical()
                    .id_salt("logscroll")
                    .stick_to_bottom(true)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for l in &self.log {
                            ui.label(RichText::new(l).monospace().size(12.0));
                        }
                    });
            });
        egui::CentralPanel::default().show(ui, |ui| {
            self.chip_panel(ui);
            ui.separator();
            self.actions(ui);
            ui.add_space(6.0);
            self.progress_row(ui);
            ui.separator();
            self.buffer_panel(ui);
        });
    }
}

fn name_of(p: &std::path::Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

fn size_str(b: u32) -> String {
    if b >= 1024 * 1024 && b.is_multiple_of(1024 * 1024) {
        format!("{} MiB", b / (1024 * 1024))
    } else {
        format!("{} KiB", b / 1024)
    }
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Small generated window icon: a dark chip with eight legs.
fn icon() -> egui::IconData {
    let n = 64usize;
    let mut rgba = vec![0u8; n * n * 4];
    let mut put = |x: usize, y: usize, c: [u8; 4]| {
        let i = (y * n + x) * 4;
        rgba[i..i + 4].copy_from_slice(&c);
    };
    for y in 0..n {
        for x in 0..n {
            let body = (16..48).contains(&x) && (10..54).contains(&y);
            let leg = ((8..16).contains(&x) || (48..56).contains(&x))
                && [14usize, 24, 34, 44]
                    .iter()
                    .any(|&l| (l..l + 5).contains(&y));
            if body {
                put(x, y, [40, 44, 52, 255]);
            } else if leg {
                put(x, y, [190, 196, 204, 255]);
            }
        }
    }
    for y in 15..21 {
        for x in 21..27 {
            put(x, y, [80, 170, 255, 255]);
        }
    }
    egui::IconData {
        rgba,
        width: n as u32,
        height: n as u32,
    }
}
