#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod theme;
mod worker;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Instant;

use eframe::egui::{
    self, text::LayoutJob, Align, Align2, Color32, FontId, Frame, Key, KeyboardShortcut, Layout,
    Margin, Modifiers, RichText, Sense, Stroke, TextFormat, Ui, Vec2,
};
use rt809h_core::chips::{self, Chip};
use rt809h_core::{DetectedChip, Voltage};
use theme::{bold, tint, Palette};
use worker::{Event, Job, Settings, Worker};

const APP_ID: &str = "rt809h-linux";
const SIDEBAR_W: f32 = 320.0;

const SC_OPEN: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
const SC_SAVE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::S);
const SC_READ: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::R);
const SC_DETECT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::F5);

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RT809H Programmer")
            .with_app_id(APP_ID)
            .with_inner_size([1120.0, 760.0])
            .with_min_inner_size([920.0, 600.0])
            .with_icon(icon()),
        ..Default::default()
    };
    eframe::run_native(
        "RT809H Programmer",
        options,
        Box::new(|cc| {
            theme::apply(&cc.egui_ctx);
            Ok(Box::new(App::new(&cc.egui_ctx)))
        }),
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
    crc: u32,
}

impl Buffer {
    fn new(data: Vec<u8>, origin: String) -> Self {
        let crc = crc32(&data);
        Self {
            data: Arc::new(data),
            origin,
            crc,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum LogKind {
    Info,
    Ok,
    Error,
}

struct LogLine {
    at: f32,
    kind: LogKind,
    text: String,
}

#[derive(Clone, Copy, PartialEq)]
enum Confirm {
    Write,
    Erase,
}

struct Running {
    label: &'static str,
    fraction: f32,
    started: Instant,
}

struct App {
    jobs: Sender<Job>,
    events: Receiver<Event>,
    cancel: Arc<AtomicBool>,
    t0: Instant,
    busy: Option<&'static str>,
    connected: Option<String>,
    detected: Option<DetectedChip>,
    contact: Option<Option<String>>,
    volt: VoltChoice,
    chip: Option<Chip>,
    chip_filter: String,
    chip_popup_open: bool,
    contact_check: bool,
    verify_after_write: bool,
    spi_mhz: u32,
    buffer: Option<Buffer>,
    running: Option<Running>,
    status: Option<(bool, String)>,
    log: Vec<LogLine>,
    log_open: bool,
    confirm: Option<Confirm>,
    goto: String,
    scroll_to_row: Option<usize>,
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
            t0: Instant::now(),
            busy: None,
            connected: None,
            detected: None,
            contact: None,
            volt: VoltChoice::Auto,
            chip: None,
            chip_filter: String::new(),
            chip_popup_open: false,
            contact_check: true,
            verify_after_write: true,
            spi_mhz: 15,
            buffer: None,
            running: None,
            status: None,
            log: Vec::new(),
            log_open: true,
            confirm: None,
            goto: String::new(),
            scroll_to_row: None,
        };
        app.start("Connecting", Job::Connect);
        if let Some(path) = std::env::args_os().nth(1) {
            app.load_path(std::path::Path::new(&path));
        }
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

    fn start(&mut self, what: &'static str, job: Job) {
        if self.busy.is_some() {
            return;
        }
        if !matches!(job, Job::Connect) {
            self.contact = None;
        }
        self.busy = Some(what);
        self.running = None;
        self.status = None;
        let _ = self.jobs.send(job);
    }

    fn push_log(&mut self, kind: LogKind, text: impl Into<String>) {
        self.log.push(LogLine {
            at: self.t0.elapsed().as_secs_f32(),
            kind,
            text: text.into(),
        });
        if self.log.len() > 1000 {
            self.log.drain(..self.log.len() - 1000);
        }
    }

    fn poll(&mut self) {
        while let Ok(ev) = self.events.try_recv() {
            match ev {
                Event::Log(m) => self.push_log(LogKind::Info, m),
                Event::Progress(label, f) => match &mut self.running {
                    Some(r) if r.label == label => r.fraction = f,
                    _ => {
                        self.running = Some(Running {
                            label,
                            fraction: f,
                            started: Instant::now(),
                        })
                    }
                },
                Event::Connected(l) => self.connected = Some(l),
                Event::Disconnected => self.connected = None,
                Event::Detected(c) => self.detected = Some(c),
                Event::Contact(c) => self.contact = Some(c),
                Event::Data(d) => {
                    let origin = match &self.detected {
                        Some(c) => format!("Read from {}", c.name()),
                        None => "Read from chip".into(),
                    };
                    self.buffer = Some(Buffer::new(d, origin));
                }
                Event::Finished(r) => {
                    let was = self.busy.take();
                    self.running = None;
                    match r {
                        Ok(m) => {
                            self.push_log(LogKind::Ok, m.clone());
                            if was != Some("Connecting") {
                                self.status = Some((true, m));
                            }
                        }
                        Err(m) => {
                            self.push_log(LogKind::Error, m.clone());
                            self.status = Some((false, m));
                        }
                    }
                }
            }
        }
    }

    fn load_path(&mut self, path: &std::path::Path) {
        match std::fs::read(path) {
            Ok(d) => {
                self.push_log(
                    LogKind::Info,
                    format!("Loaded {} ({})", path.display(), bytes_str(d.len())),
                );
                self.buffer = Some(Buffer::new(d, name_of(path)));
                self.scroll_to_row = Some(0);
            }
            Err(e) => {
                let m = format!("Cannot open {}: {e}", path.display());
                self.push_log(LogKind::Error, m.clone());
                self.status = Some((false, m));
            }
        }
    }

    fn open_file(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Binary images", &["bin", "rom", "img", "BIN", "ROM"])
            .add_filter("All files", &["*"])
            .pick_file()
        {
            self.load_path(&path);
        }
    }

    fn save_file(&mut self) {
        let Some(buf) = &self.buffer else { return };
        let default = match &self.detected {
            Some(c) => format!("{}.bin", c.name().replace([' ', '/', '(', ')'], "_")),
            None => "dump.bin".into(),
        };
        let data = buf.data.clone();
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Binary images", &["bin"])
            .set_file_name(default)
            .save_file()
        {
            match std::fs::write(&path, data.as_slice()) {
                Ok(()) => {
                    let m = format!("Saved {}", path.display());
                    self.push_log(LogKind::Ok, m.clone());
                    self.status = Some((true, m));
                }
                Err(e) => {
                    let m = format!("Cannot save: {e}");
                    self.push_log(LogKind::Error, m.clone());
                    self.status = Some((false, m));
                }
            }
        }
    }

    fn handle_input(&mut self, ctx: &egui::Context) {
        let idle = self.busy.is_none() && self.confirm.is_none();
        let (open, save, read, detect) = ctx.input_mut(|i| {
            (
                i.consume_shortcut(&SC_OPEN),
                i.consume_shortcut(&SC_SAVE),
                i.consume_shortcut(&SC_READ),
                i.consume_shortcut(&SC_DETECT),
            )
        });
        if idle && open {
            self.open_file();
        }
        if save && self.buffer.is_some() {
            self.save_file();
        }
        if idle && read {
            let s = self.settings();
            self.start("Reading", Job::Read(s));
        }
        if idle && detect {
            let s = self.settings();
            self.start("Detecting", Job::Detect(s));
        }
        let dropped: Vec<std::path::PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if idle {
            if let Some(p) = dropped.first() {
                self.load_path(p);
            }
        }
    }

    // ----------------------------------------------------------------- header

    fn header(&mut self, ui: &mut Ui, p: &Palette) {
        ui.horizontal(|ui| {
            logo(ui, p);
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(
                    RichText::new("RT809H Programmer")
                        .family(bold())
                        .size(16.0)
                        .color(p.text),
                );
                ui.label(
                    RichText::new("25-series SPI flash")
                        .size(12.0)
                        .color(p.dim),
                );
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let idle = self.busy.is_none();
                if ui
                    .add_enabled(idle, egui::Button::new("Reconnect"))
                    .on_hover_text("Re-open the USB connection")
                    .clicked()
                {
                    self.start("Connecting", Job::Connect);
                }
                let (color, text) = match (&self.connected, self.busy) {
                    (_, Some("Connecting")) => (p.warn, "Connecting…".to_string()),
                    (Some(_), _) => (p.ok, "Programmer connected".to_string()),
                    (None, _) => (p.err, "Programmer not found".to_string()),
                };
                let r = pill(ui, p, color, &text);
                match &self.connected {
                    Some(l) => r.on_hover_text(l),
                    None => r.on_hover_text(
                        "Plug in the RT809H. If it is plugged in, check that the udev rule is installed and re-plug it.",
                    ),
                };
            });
        });
    }

    // ---------------------------------------------------------------- sidebar

    fn sidebar(&mut self, ui: &mut Ui, p: &Palette) {
        egui::ScrollArea::vertical()
            .id_salt("side")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.chip_card(ui, p);
                ui.add_space(12.0);
                self.settings_card(ui, p);
                ui.add_space(12.0);
                self.options_card(ui, p);
            });
    }

    fn chip_card(&mut self, ui: &mut Ui, p: &Palette) {
        card(ui, p, |ui| {
            section(ui, p, "CHIP");
            match &self.detected {
                Some(c) => {
                    ui.label(
                        RichText::new(c.name())
                            .family(bold())
                            .size(20.0)
                            .color(p.text),
                    );
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        tag(ui, p, &format!("ID {}", c.id));
                        tag(ui, p, &size_str(c.geometry.size));
                        tag(ui, p, c.voltage.label());
                    });
                }
                None => {
                    ui.label(
                        RichText::new("No chip detected")
                            .family(bold())
                            .size(20.0)
                            .color(p.faint),
                    );
                    ui.label(
                        RichText::new(
                            "Put the chip at the bottom of the socket with pin 1 facing up, close the lever and press Detect.",
                        )
                        .size(12.5)
                        .color(p.dim),
                    );
                }
            }
            ui.add_space(4.0);
            let (dot, text, color) = match &self.contact {
                None => (p.faint, "Pin contact not checked yet".to_string(), p.dim),
                Some(None) => (p.ok, "All 8 pins have good contact".to_string(), p.text),
                Some(Some(problem)) => (p.err, problem.clone(), p.err),
            };
            ui.horizontal_top(|ui| {
                let (r, _) = ui.allocate_exact_size(Vec2::new(10.0, 18.0), Sense::hover());
                ui.painter().circle_filled(r.center(), 4.0, dot);
                ui.add(egui::Label::new(RichText::new(text).size(12.5).color(color)).wrap());
            });
            ui.add_space(6.0);
            let idle = self.busy.is_none();
            ui.horizontal(|ui| {
                let w = ui.available_width();
                if ui
                    .add_enabled(
                        idle,
                        primary_button(p, "Detect chip").min_size(Vec2::new(w * 0.6, 34.0)),
                    )
                    .on_hover_text("Check the pins and identify the chip (F5)")
                    .clicked()
                {
                    let s = self.settings();
                    self.start("Detecting", Job::Detect(s));
                }
                if ui
                    .add_enabled(
                        idle,
                        egui::Button::new("Pin test")
                            .min_size(Vec2::new(ui.available_width(), 34.0)),
                    )
                    .on_hover_text("Only check that every chip pin touches the socket")
                    .clicked()
                {
                    let s = self.settings();
                    self.start("Checking contact", Job::Contact(s));
                }
            });
        });
    }

    fn settings_card(&mut self, ui: &mut Ui, p: &Palette) {
        card(ui, p, |ui| {
            section(ui, p, "CHIP TYPE");
            let label = match &self.chip {
                Some(c) => c.display_name(),
                None => "Auto detect".into(),
            };
            let width = ui.available_width();
            // keep the popup below the box: size it to the room that is left
            let below = ui.ctx().content_rect().bottom() - ui.cursor().top() - 60.0;
            let list_h = (below - 56.0).clamp(140.0, 380.0);
            let popup = egui::ComboBox::from_id_salt("chip_sel")
                .width(width)
                .height(list_h + 56.0)
                .selected_text(label)
                // clicks inside (e.g. into the search field) must not close it
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .show_ui(ui, |ui| {
                    ui.set_min_width(width);
                    let search = ui.add(
                        egui::TextEdit::singleline(&mut self.chip_filter)
                            .hint_text("Type to search, e.g. W25Q128 or MX25L")
                            .desired_width(f32::INFINITY),
                    );
                    if !self.chip_popup_open {
                        // popup just opened: start typing right away
                        search.request_focus();
                        self.chip_popup_open = true;
                    }
                    let matches = chips::search(&self.chip_filter);
                    if search.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        // Enter picks the first match (or auto detect)
                        self.chip = if self.chip_filter.trim().is_empty() {
                            None
                        } else {
                            matches.first().map(|c| **c).or(self.chip)
                        };
                        ui.close();
                    }
                    ui.add_space(4.0);
                    egui::ScrollArea::vertical()
                        .id_salt("chip_list")
                        // fixed height so the popup does not shrink while filtering
                        .min_scrolled_height(list_h)
                        .max_height(list_h)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            if ui
                                .selectable_label(self.chip.is_none(), "Auto detect (recommended)")
                                .clicked()
                            {
                                self.chip = None;
                                ui.close();
                            }
                            if matches.is_empty() {
                                ui.horizontal(|ui| {
                                    ui.add_space(12.0);
                                    ui.label(RichText::new("No matching chip").color(p.dim));
                                });
                            }
                            for c in matches.iter().take(250) {
                                let sel = self
                                    .chip
                                    .map(|x| x.id == c.id && x.name == c.name)
                                    .unwrap_or(false);
                                let mut job = LayoutJob::default();
                                job.append(
                                    &c.display_name(),
                                    0.0,
                                    TextFormat::simple(FontId::proportional(14.0), p.text),
                                );
                                job.append(
                                    &format!("   {} · {}", size_str(c.size), c.voltage),
                                    0.0,
                                    TextFormat::simple(FontId::proportional(12.0), p.dim),
                                );
                                if ui.selectable_label(sel, job).clicked() {
                                    self.chip = Some(**c);
                                    ui.close();
                                }
                            }
                        });
                });
            if popup.inner.is_none() {
                self.chip_popup_open = false;
            }
            ui.add_space(8.0);
            section(ui, p, "SUPPLY VOLTAGE");
            match self.chip {
                Some(c) => {
                    ui.label(
                        RichText::new(format!("{} (set by the selected chip)", c.voltage))
                            .color(p.dim),
                    );
                }
                None => {
                    segmented(ui, p, &mut self.volt);
                    let hint = match self.volt {
                        VoltChoice::Auto => {
                            "Starts at 1.8 V and steps up to 3.3 V, so low-voltage chips are never overdriven."
                        }
                        VoltChoice::Fixed(Voltage::V3_3) => {
                            "Only use 3.3 V if you are sure the chip is rated for it."
                        }
                        VoltChoice::Fixed(_) => "The chip is always powered at this voltage.",
                    };
                    ui.label(RichText::new(hint).size(12.0).color(p.dim));
                }
            }
        });
    }

    fn options_card(&mut self, ui: &mut Ui, p: &Palette) {
        card(ui, p, |ui| {
            section(ui, p, "OPTIONS");
            ui.checkbox(&mut self.contact_check, "Check pin contact first");
            ui.checkbox(&mut self.verify_after_write, "Verify after writing");
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.label("SPI clock");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add(
                        egui::Slider::new(&mut self.spi_mhz, 1..=30)
                            .suffix(" MHz")
                            .show_value(true),
                    )
                    .on_hover_text("Lower it if reads are unstable (long wires, adapters)");
                });
            });
        });
    }

    // ------------------------------------------------------------ operations

    fn operations(&mut self, ui: &mut Ui, p: &Palette) {
        let idle = self.busy.is_none();
        let has_buf = self.buffer.is_some();
        let need_buf = "Open a file or read a chip first";
        let tiles: [(&str, &str, bool, Option<&str>, u8); 5] = [
            ("Read", "Copy the chip into the buffer", false, None, 0),
            (
                "Write",
                "Erase and program the buffer",
                true,
                (!has_buf).then_some(need_buf),
                1,
            ),
            (
                "Verify",
                "Compare the chip with the buffer",
                false,
                (!has_buf).then_some(need_buf),
                2,
            ),
            ("Erase", "Erase the whole chip", true, None, 3),
            (
                "Blank check",
                "Check that the chip is empty",
                false,
                None,
                4,
            ),
        ];
        let gap = 10.0;
        let cols = if ui.available_width() > 760.0 { 5 } else { 3 };
        let w = (ui.available_width() - gap * (cols as f32 - 1.0)) / cols as f32;
        let mut clicked = None;
        for row in tiles.chunks(cols) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for (title, desc, danger, blocked, id) in row {
                    let enabled = idle && blocked.is_none();
                    let r = tile(ui, p, w, title, desc, *danger, enabled);
                    let r = match blocked {
                        Some(why) => r.on_disabled_hover_text(*why),
                        None => r,
                    };
                    if r.clicked() {
                        clicked = Some(*id);
                    }
                }
            });
        }
        let s = self.settings();
        match clicked {
            Some(0) => self.start("Reading", Job::Read(s)),
            Some(1) => self.confirm = Some(Confirm::Write),
            Some(2) => {
                let d = self.buffer.as_ref().unwrap().data.clone();
                self.start("Verifying", Job::Verify(s, d));
            }
            Some(3) => self.confirm = Some(Confirm::Erase),
            Some(4) => self.start("Blank check", Job::Blank(s)),
            _ => {}
        }
    }

    fn status_card(&mut self, ui: &mut Ui, p: &Palette) {
        let (fill, stroke) = match (&self.busy, &self.status) {
            (None, Some((true, _))) => (tint(p.surface, p.ok, 0.12), tint(p.border, p.ok, 0.5)),
            (None, Some((false, _))) => (tint(p.surface, p.err, 0.12), tint(p.border, p.err, 0.5)),
            _ => (p.surface, p.border),
        };
        Frame::new()
            .fill(fill)
            .stroke(Stroke::new(1.0, stroke))
            .corner_radius(10)
            .inner_margin(Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                if let Some(what) = self.busy {
                    let (label, f, started) = match &self.running {
                        Some(r) => (r.label, r.fraction, Some(r.started)),
                        None => (what, 0.0, None),
                    };
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(16.0).color(p.accent));
                        ui.label(
                            RichText::new(format!("{label}…"))
                                .family(bold())
                                .color(p.text),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.button("Cancel").clicked() {
                                self.cancel.store(true, Ordering::SeqCst);
                                self.push_log(LogKind::Info, "Cancelling…");
                            }
                            if self.running.is_some() {
                                ui.label(
                                    RichText::new(format!("{:.0} %", f * 100.0))
                                        .family(bold())
                                        .color(p.text),
                                );
                            }
                        });
                    });
                    ui.add(
                        egui::ProgressBar::new(f)
                            .desired_height(8.0)
                            .fill(p.accent)
                            .corner_radius(4)
                            .animate(self.running.is_none()),
                    );
                    let detail = match started {
                        Some(t) if f > 0.02 => {
                            let el = t.elapsed().as_secs_f32();
                            format!(
                                "{} elapsed · about {} left",
                                dur_str(el),
                                dur_str(el / f - el)
                            )
                        }
                        Some(t) => format!("{} elapsed", dur_str(t.elapsed().as_secs_f32())),
                        None => "Preparing…".into(),
                    };
                    ui.label(RichText::new(detail).size(12.0).color(p.dim));
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(250));
                } else if let Some((ok, msg)) = &self.status {
                    ui.horizontal_top(|ui| {
                        status_icon(ui, if *ok { p.ok } else { p.err }, *ok);
                        ui.add(
                            egui::Label::new(
                                RichText::new(msg.as_str()).family(bold()).color(if *ok {
                                    p.text
                                } else {
                                    p.err
                                }),
                            )
                            .wrap(),
                        );
                    });
                } else {
                    ui.label(RichText::new("Ready").family(bold()).color(p.text));
                    ui.label(
                        RichText::new("Insert a chip and press Detect, or open a file to write.")
                            .size(12.5)
                            .color(p.dim),
                    );
                }
            });
    }

    // ----------------------------------------------------------------- buffer

    fn buffer_card(&mut self, ui: &mut Ui, p: &Palette) {
        Frame::new()
            .fill(p.surface)
            .stroke(Stroke::new(1.0, p.border))
            .corner_radius(10)
            .inner_margin(Margin::same(14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_height(ui.available_height());
                ui.horizontal(|ui| {
                    section(ui, p, "BUFFER");
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let idle = self.busy.is_none();
                        let has = self.buffer.is_some();
                        if ui
                            .add_enabled(has && idle, egui::Button::new("Clear"))
                            .clicked()
                        {
                            self.buffer = None;
                        }
                        if ui
                            .add_enabled(has, egui::Button::new("Save…"))
                            .on_hover_text("Save the buffer to a file (Ctrl+S)")
                            .clicked()
                        {
                            self.save_file();
                        }
                        if ui
                            .add_enabled(idle, egui::Button::new("Open…"))
                            .on_hover_text("Load a file into the buffer (Ctrl+O)")
                            .clicked()
                        {
                            self.open_file();
                        }
                    });
                });
                if self.buffer.is_none() {
                    self.empty_buffer(ui, p);
                    return;
                }
                self.buffer_info(ui, p);
                ui.add_space(4.0);
                self.hex_view(ui, p);
            });
    }

    fn empty_buffer(&mut self, ui: &mut Ui, p: &Palette) {
        ui.with_layout(Layout::top_down(Align::Center), |ui| {
            ui.add_space((ui.available_height() / 2.0 - 70.0).max(10.0));
            ui.label(
                RichText::new("No data")
                    .family(bold())
                    .size(18.0)
                    .color(p.dim),
            );
            ui.label(
                RichText::new("Read a chip, open a file, or drop a .bin file onto this window.")
                    .color(p.dim),
            );
            ui.add_space(8.0);
            let idle = self.busy.is_none();
            ui.horizontal(|ui| {
                let total = 120.0 * 2.0 + ui.spacing().item_spacing.x;
                ui.add_space(((ui.available_width() - total) / 2.0).max(0.0));
                if ui
                    .add_enabled(
                        idle,
                        primary_button(p, "Read chip").min_size(Vec2::new(120.0, 32.0)),
                    )
                    .clicked()
                {
                    let s = self.settings();
                    self.start("Reading", Job::Read(s));
                }
                if ui
                    .add_enabled(
                        idle,
                        egui::Button::new("Open file…").min_size(Vec2::new(120.0, 32.0)),
                    )
                    .clicked()
                {
                    self.open_file();
                }
            });
        });
    }

    fn buffer_info(&mut self, ui: &mut Ui, p: &Palette) {
        let Some(b) = &self.buffer else { return };
        let len = b.data.len();
        ui.horizontal(|ui| {
            ui.label(RichText::new(&b.origin).family(bold()).color(p.text));
            ui.label(
                RichText::new(format!("{}  ·  CRC32 {:08X}", bytes_str(len), b.crc)).color(p.dim),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let go = ui.add(
                    egui::TextEdit::singleline(&mut self.goto)
                        .hint_text("Go to address")
                        .desired_width(130.0)
                        .font(egui::TextStyle::Monospace),
                );
                if go.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    let t = self
                        .goto
                        .trim()
                        .trim_start_matches("0x")
                        .trim_start_matches("0X");
                    if let Ok(a) = usize::from_str_radix(t, 16) {
                        self.scroll_to_row = Some(a.min(len.saturating_sub(1)) / 16);
                    }
                }
            });
        });
        if let Some(c) = &self.detected {
            let chip = c.geometry.size as usize;
            if len > chip {
                warn_line(
                    ui,
                    p.err,
                    &format!(
                        "The buffer is larger than the chip ({}). It cannot be written.",
                        size_str(c.geometry.size)
                    ),
                );
            } else if len < chip {
                warn_line(
                    ui,
                    p.warn,
                    &format!(
                        "The buffer is smaller than the chip ({}). Writing only changes the first {}.",
                        size_str(c.geometry.size),
                        bytes_str(len)
                    ),
                );
            }
        }
    }

    fn hex_view(&mut self, ui: &mut Ui, p: &Palette) {
        let Some(b) = &self.buffer else { return };
        let data = b.data.clone();
        let mono = FontId::monospace(13.0);
        let row_h = ui.fonts_mut(|f| f.row_height(&mono)) + 2.0;
        let rows = data.len().div_ceil(16);

        Frame::new()
            .fill(tint(p.surface, p.bg, 0.5))
            .corner_radius(6)
            .inner_margin(Margin::symmetric(0, 4))
            .show(ui, |ui| {
                let mut head = String::from("Offset    ");
                for i in 0..16 {
                    head.push_str(&format!("{i:02X} "));
                    if i == 7 {
                        head.push(' ');
                    }
                }
                head.push_str(" ASCII");
                let mut job = LayoutJob::default();
                job.append(&head, 0.0, TextFormat::simple(mono.clone(), p.faint));
                paint_row(ui, row_h + 6.0, job);
                let mut area = egui::ScrollArea::vertical()
                    .id_salt("hex")
                    .auto_shrink([false, false]);
                if let Some(r) = self.scroll_to_row.take() {
                    area = area.vertical_scroll_offset(r as f32 * row_h);
                }
                // rows are packed without gaps; show_rows must know that, or it
                // reserves space for gaps and leaves the bottom of the view empty
                ui.spacing_mut().item_spacing.y = 0.0;
                area.show_rows(ui, row_h, rows, |ui, range| {
                    for r in range {
                        let off = r * 16;
                        let line = &data[off..(off + 16).min(data.len())];
                        let mut job = LayoutJob::default();
                        let fmt = |c: Color32| TextFormat::simple(mono.clone(), c);
                        job.append(&format!("{off:08X}  "), 0.0, fmt(p.accent));
                        for (i, &v) in line.iter().enumerate() {
                            let c = match v {
                                0xFF => p.faint,
                                0x00 => tint(p.faint, p.dim, 0.5),
                                _ => p.text,
                            };
                            let sep = if i == 7 { "  " } else { " " };
                            job.append(&format!("{v:02X}{sep}"), 0.0, fmt(c));
                        }
                        for i in line.len()..16 {
                            job.append(if i == 7 { "    " } else { "   " }, 0.0, fmt(p.faint));
                        }
                        let asc: String = line
                            .iter()
                            .map(|&v| {
                                if (0x20..0x7F).contains(&v) {
                                    v as char
                                } else {
                                    '.'
                                }
                            })
                            .collect();
                        job.append(&format!(" {asc}"), 0.0, fmt(p.dim));
                        paint_row(ui, row_h, job);
                    }
                });
            });
    }

    // -------------------------------------------------------------------- log

    fn log_panel(&mut self, ui: &mut Ui, p: &Palette) {
        ui.horizontal(|ui| {
            let r = chevron_label(ui, p, "Activity", self.log_open);
            if r.clicked() {
                self.log_open = !self.log_open;
            }
            if let Some(last) = self.log.last().filter(|_| !self.log_open) {
                ui.label(RichText::new(&last.text).size(12.5).color(p.dim));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if self.log_open {
                    if ui.small_button("Clear").clicked() {
                        self.log.clear();
                    }
                    if ui.small_button("Copy").clicked() {
                        let text: Vec<String> = self
                            .log
                            .iter()
                            .map(|l| format!("[{}] {}", dur_str(l.at), l.text))
                            .collect();
                        ui.ctx().copy_text(text.join("\n"));
                    }
                }
            });
        });
        if !self.log_open {
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt("logscroll")
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                let mono = FontId::monospace(12.5);
                for l in &self.log {
                    let color = match l.kind {
                        LogKind::Info => p.text,
                        LogKind::Ok => p.ok,
                        LogKind::Error => p.err,
                    };
                    let mut job = LayoutJob::default();
                    job.append(
                        &format!("{:>6}  ", dur_str(l.at)),
                        0.0,
                        TextFormat::simple(mono.clone(), p.faint),
                    );
                    job.append(&l.text, 0.0, TextFormat::simple(mono.clone(), color));
                    ui.label(job);
                }
            });
    }

    // ---------------------------------------------------------------- dialogs

    fn confirm_dialog(&mut self, ctx: &egui::Context, p: &Palette) {
        let Some(kind) = self.confirm else { return };
        let chip = self
            .detected
            .as_ref()
            .map(|c| c.name())
            .unwrap_or_else(|| "the chip".into());
        let (title, body, action) = match kind {
            Confirm::Erase => (
                "Erase the whole chip?",
                format!("All data on {chip} will be lost. This cannot be undone."),
                "Erase chip",
            ),
            Confirm::Write => {
                let b = self.buffer.as_ref().unwrap();
                (
                    "Write the buffer to the chip?",
                    format!(
                        "The buffer ({}, {}) will be written to {chip}. The area is erased first{}.",
                        bytes_str(b.data.len()),
                        lower_first(&b.origin),
                        if self.verify_after_write {
                            " and verified afterwards"
                        } else {
                            ""
                        }
                    ),
                    "Write chip",
                )
            }
        };
        let mut go = false;
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::unique("rt809h-confirm"))
            .frame(
                Frame::new()
                    .fill(p.surface)
                    .stroke(Stroke::new(1.0, p.border))
                    .corner_radius(12)
                    .inner_margin(Margin::same(20)),
            )
            .show(ctx, |ui| {
                ui.set_width(420.0);
                ui.label(RichText::new(title).family(bold()).size(17.0).color(p.text));
                ui.add_space(4.0);
                ui.add(egui::Label::new(RichText::new(body).color(p.dim)).wrap());
                ui.add_space(14.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add(
                            egui::Button::new(RichText::new(action).color(Color32::WHITE))
                                .fill(p.err)
                                .min_size(Vec2::new(110.0, 32.0)),
                        )
                        .clicked()
                    {
                        go = true;
                    }
                    if ui
                        .add(egui::Button::new("Cancel").min_size(Vec2::new(90.0, 32.0)))
                        .clicked()
                    {
                        close = true;
                    }
                });
            });
        if modal.should_close() {
            close = true;
        }
        if go {
            self.confirm = None;
            let s = self.settings();
            match kind {
                Confirm::Erase => self.start("Erasing", Job::Erase(s)),
                Confirm::Write => {
                    let d = self.buffer.as_ref().unwrap().data.clone();
                    let v = self.verify_after_write;
                    self.start("Writing", Job::Write(s, d, v));
                }
            }
        } else if close {
            self.confirm = None;
        }
    }

    fn drop_overlay(&self, ctx: &egui::Context, p: &Palette) {
        if ctx.input(|i| i.raw.hovered_files.is_empty()) {
            return;
        }
        let rect = ctx.content_rect();
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::unique("rt809h-drop"),
        ));
        painter.rect_filled(rect, 0, Color32::from_black_alpha(160));
        painter.rect_stroke(
            rect.shrink(16.0),
            12,
            Stroke::new(2.0, p.accent),
            egui::StrokeKind::Inside,
        );
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "Drop to load into the buffer",
            FontId::new(22.0, bold()),
            Color32::WHITE,
        );
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.poll();
        let ctx = ui.ctx().clone();
        self.handle_input(&ctx);
        let p = Palette::of(ui);

        egui::Panel::top("header")
            .frame(
                Frame::new()
                    .fill(p.surface)
                    .inner_margin(Margin::symmetric(16, 10)),
            )
            .show(ui, |ui| self.header(ui, &p));

        let log = egui::Panel::bottom("log").frame(
            Frame::new()
                .fill(p.surface)
                .inner_margin(Margin::symmetric(16, 8)),
        );
        let log = if self.log_open {
            log.resizable(true).default_size(130.0).min_size(80.0)
        } else {
            log.resizable(false).exact_size(36.0)
        };
        log.show(ui, |ui| self.log_panel(ui, &p));

        egui::Panel::left("side")
            .exact_size(SIDEBAR_W)
            .resizable(false)
            .frame(Frame::new().fill(p.bg).inner_margin(Margin {
                left: 16,
                right: 8,
                top: 16,
                bottom: 16,
            }))
            .show(ui, |ui| self.sidebar(ui, &p));

        egui::CentralPanel::default()
            .frame(Frame::new().fill(p.bg).inner_margin(Margin {
                left: 8,
                right: 16,
                top: 16,
                bottom: 16,
            }))
            .show(ui, |ui| {
                self.operations(ui, &p);
                ui.add_space(4.0);
                self.status_card(ui, &p);
                ui.add_space(4.0);
                self.buffer_card(ui, &p);
            });

        self.confirm_dialog(&ctx, &p);
        self.drop_overlay(&ctx, &p);
    }
}

// ------------------------------------------------------------------ widgets

fn card<R>(ui: &mut Ui, p: &Palette, add: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(10)
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

fn section(ui: &mut Ui, p: &Palette, title: &str) {
    ui.label(
        RichText::new(title)
            .family(bold())
            .size(11.0)
            .color(p.dim)
            .extra_letter_spacing(0.8),
    );
}

fn tag(ui: &mut Ui, p: &Palette, text: &str) {
    Frame::new()
        .fill(p.surface2)
        .corner_radius(5)
        .inner_margin(Margin::symmetric(7, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(12.0).color(p.text));
        });
}

fn pill(ui: &mut Ui, p: &Palette, color: Color32, text: &str) -> egui::Response {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), FontId::proportional(13.0), p.text);
    let size = Vec2::new(galley.size().x + 36.0, 28.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter();
    painter.rect(
        rect,
        14,
        tint(p.surface, color, 0.14),
        Stroke::new(1.0, tint(p.surface, color, 0.4)),
        egui::StrokeKind::Inside,
    );
    painter.circle_filled(egui::pos2(rect.left() + 15.0, rect.center().y), 4.0, color);
    let pos = egui::pos2(rect.left() + 26.0, rect.center().y - galley.size().y / 2.0);
    painter.galley(pos, galley, p.text);
    resp
}

fn primary_button<'a>(p: &Palette, text: &'a str) -> egui::Button<'a> {
    egui::Button::new(RichText::new(text).family(bold()).color(p.on_accent))
        .fill(p.accent)
        .stroke(Stroke::NONE)
}

fn segmented(ui: &mut Ui, p: &Palette, value: &mut VoltChoice) {
    let options = [
        (VoltChoice::Auto, "Auto"),
        (VoltChoice::Fixed(Voltage::V1_8), "1.8 V"),
        (VoltChoice::Fixed(Voltage::V2_5), "2.5 V"),
        (VoltChoice::Fixed(Voltage::V3_3), "3.3 V"),
    ];
    let h = 32.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::hover());
    ui.painter().rect(
        rect,
        8,
        p.surface2,
        Stroke::new(1.0, p.border),
        egui::StrokeKind::Inside,
    );
    let w = rect.width() / options.len() as f32;
    for (i, (v, label)) in options.iter().enumerate() {
        let r = egui::Rect::from_min_size(rect.min + Vec2::new(i as f32 * w, 0.0), Vec2::new(w, h))
            .shrink(3.0);
        let resp = ui.interact(r, ui.scope_id().with(("volt", i)), Sense::click());
        let selected = value == v;
        if selected {
            ui.painter().rect_filled(r, 6, p.accent);
        } else if resp.hovered() {
            ui.painter()
                .rect_filled(r, 6, tint(p.surface2, p.text, 0.07));
        }
        ui.painter().text(
            r.center(),
            Align2::CENTER_CENTER,
            *label,
            FontId::new(
                13.5,
                if selected {
                    bold()
                } else {
                    egui::FontFamily::Proportional
                },
            ),
            if selected { p.on_accent } else { p.text },
        );
        if resp.clicked() {
            *value = *v;
        }
    }
}

fn tile(
    ui: &mut Ui,
    p: &Palette,
    w: f32,
    title: &str,
    desc: &str,
    danger: bool,
    enabled: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(w, 80.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let hovered = enabled && resp.hovered();
    let pressed = enabled && resp.is_pointer_button_down_on();
    let accent = if danger { p.err } else { p.accent };
    let fill = if pressed {
        tint(p.surface, accent, 0.18)
    } else if hovered {
        tint(p.surface, accent, 0.08)
    } else {
        p.surface
    };
    let stroke = if hovered {
        tint(p.border, accent, 0.7)
    } else {
        p.border
    };
    let painter = ui.painter();
    painter.rect(
        rect,
        10,
        fill,
        Stroke::new(1.0, stroke),
        egui::StrokeKind::Inside,
    );
    // accent bar on the left
    let bar = egui::Rect::from_min_size(
        rect.min + Vec2::new(0.0, 14.0),
        Vec2::new(3.0, rect.height() - 28.0),
    );
    let alpha = if enabled { 1.0 } else { 0.35 };
    painter.rect_filled(bar, 2, accent.gamma_multiply(alpha));
    let title_c = if danger { p.err } else { p.text };
    painter.text(
        rect.min + Vec2::new(16.0, 16.0),
        Align2::LEFT_TOP,
        title,
        FontId::new(15.5, bold()),
        title_c.gamma_multiply(alpha),
    );
    let galley = painter.layout(
        desc.to_string(),
        FontId::proportional(12.0),
        p.dim.gamma_multiply(alpha),
        w - 26.0,
    );
    painter.galley(rect.min + Vec2::new(16.0, 40.0), galley, p.dim);
    if enabled {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp
    }
}

/// One left-aligned, fixed-height line of the hex view.
fn paint_row(ui: &mut Ui, h: f32, job: LayoutJob) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::hover());
    let galley = ui.painter().layout_job(job);
    let y = rect.center().y - galley.size().y / 2.0;
    ui.painter()
        .galley(egui::pos2(rect.left() + 12.0, y), galley, Color32::WHITE);
}

fn chevron_label(ui: &mut Ui, p: &Palette, text: &str, open: bool) -> egui::Response {
    let font = FontId::new(14.0, bold());
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, p.dim);
    let size = Vec2::new(galley.size().x + 18.0, galley.size().y.max(16.0));
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let painter = ui.painter();
    painter.galley(
        rect.left_center() - Vec2::new(0.0, galley.size().y / 2.0),
        galley,
        p.dim,
    );
    let c = egui::pos2(rect.right() - 6.0, rect.center().y);
    let pts = if open {
        vec![
            c + Vec2::new(-4.0, -2.0),
            c + Vec2::new(4.0, -2.0),
            c + Vec2::new(0.0, 3.0),
        ]
    } else {
        vec![
            c + Vec2::new(-2.0, -4.0),
            c + Vec2::new(-2.0, 4.0),
            c + Vec2::new(3.0, 0.0),
        ]
    };
    painter.add(egui::Shape::convex_polygon(pts, p.dim, Stroke::NONE));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn status_icon(ui: &mut Ui, color: Color32, ok: bool) {
    let (r, _) = ui.allocate_exact_size(Vec2::splat(20.0), Sense::hover());
    let c = r.center();
    let painter = ui.painter();
    painter.circle_filled(c, 9.0, color);
    let s = Stroke::new(2.0, Color32::WHITE);
    if ok {
        painter.line_segment([c + Vec2::new(-4.0, 0.0), c + Vec2::new(-1.0, 3.5)], s);
        painter.line_segment([c + Vec2::new(-1.0, 3.5), c + Vec2::new(4.5, -3.5)], s);
    } else {
        painter.line_segment([c + Vec2::new(-3.5, -3.5), c + Vec2::new(3.5, 3.5)], s);
        painter.line_segment([c + Vec2::new(3.5, -3.5), c + Vec2::new(-3.5, 3.5)], s);
    }
}

fn warn_line(ui: &mut Ui, color: Color32, text: &str) {
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(Vec2::new(10.0, 16.0), Sense::hover());
        ui.painter().circle_filled(r.center(), 4.0, color);
        ui.label(RichText::new(text).size(12.5).color(color));
    });
}

fn logo(ui: &mut Ui, p: &Palette) {
    let (r, _) = ui.allocate_exact_size(Vec2::splat(34.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(r, 8, p.accent);
    let body = egui::Rect::from_center_size(r.center(), Vec2::new(14.0, 18.0));
    let leg = Stroke::new(2.0, p.on_accent);
    for i in 0..4 {
        let y = body.top() + 3.0 + i as f32 * 4.0;
        painter.line_segment(
            [egui::pos2(body.left() - 4.0, y), egui::pos2(body.left(), y)],
            leg,
        );
        painter.line_segment(
            [
                egui::pos2(body.right(), y),
                egui::pos2(body.right() + 4.0, y),
            ],
            leg,
        );
    }
    painter.rect_filled(body, 2, p.on_accent);
    painter.circle_filled(body.left_top() + Vec2::new(3.5, 3.5), 1.6, p.accent);
}

// ------------------------------------------------------------------ helpers

fn name_of(p: &std::path::Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

fn size_str(b: u32) -> String {
    if b >= 1024 * 1024 && b.is_multiple_of(1024 * 1024) {
        format!("{} MiB", b / (1024 * 1024))
    } else if b >= 1024 {
        format!("{} KiB", b / 1024)
    } else {
        format!("{b} bytes")
    }
}

fn bytes_str(n: usize) -> String {
    if n >= 1024 && n.is_multiple_of(1024) {
        size_str(n as u32)
    } else {
        format!("{n} bytes")
    }
}

/// "Read from X" -> "read from X"; file names stay unchanged.
fn lower_first(s: &str) -> String {
    match s.strip_prefix("Read ") {
        Some(rest) => format!("read {rest}"),
        None => s.to_string(),
    }
}

fn dur_str(secs: f32) -> String {
    let s = secs.max(0.0).round() as u32;
    format!("{}:{:02}", s / 60, s % 60)
}

fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, t) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xEDB8_8320
            } else {
                c >> 1
            };
        }
        *t = c;
    }
    !data.iter().fold(0xFFFF_FFFFu32, |crc, &b| {
        table[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8)
    })
}

/// Generated window icon: a white chip on a blue rounded square.
fn icon() -> egui::IconData {
    let n = 64usize;
    let mut rgba = vec![0u8; n * n * 4];
    let inside = |x: usize, y: usize| {
        let r = 12.0f32;
        let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
        let cx = fx.clamp(r, n as f32 - r);
        let cy = fy.clamp(r, n as f32 - r);
        (fx - cx).powi(2) + (fy - cy).powi(2) <= r * r
    };
    for y in 0..n {
        for x in 0..n {
            let i = (y * n + x) * 4;
            if !inside(x, y) {
                continue;
            }
            let body = (22..42).contains(&x) && (16..48).contains(&y);
            let leg = ((15..22).contains(&x) || (42..49).contains(&x))
                && [19usize, 26, 33, 40]
                    .iter()
                    .any(|&l| (l..l + 3).contains(&y));
            let dot = (25..29).contains(&x) && (19..23).contains(&y);
            let c = if dot {
                [0x4f, 0x8c, 0xff, 255]
            } else if body || leg {
                [255, 255, 255, 255]
            } else {
                [0x4f, 0x8c, 0xff, 255]
            };
            rgba[i..i + 4].copy_from_slice(&c);
        }
    }
    egui::IconData {
        rgba,
        width: n as u32,
        height: n as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::crc32;

    #[test]
    fn crc32_matches_reference() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
