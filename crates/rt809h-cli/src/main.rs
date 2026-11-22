use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};
use rt809h_core::session::first_difference;
use rt809h_core::vendor_db::{self, Driver};
use rt809h_core::{DetectedChip, Error, Programmer, Result, Voltage};

#[derive(Parser)]
#[command(
    name = "rt809h",
    version,
    about = "RT809H programmer for SPI flash and I2C EEPROMs"
)]
struct Cli {
    /// Chip supply voltage (auto ramps 1.8 -> 2.5 -> 3.3 V and then uses the
    /// chip's rated voltage).
    #[arg(long, short = 'v', value_enum, default_value_t = Volt::Auto, global = true)]
    voltage: Volt,
    /// Skip the socket contact check.
    #[arg(long, global = true)]
    no_contact_check: bool,
    /// SPI clock in MHz for data transfers.
    #[arg(long, default_value_t = 10, global = true)]
    mhz: u32,
    /// Part name (see `rt809h chips`). Required for I2C EEPROMs, which
    /// cannot be identified automatically.
    #[arg(long, short = 'c', global = true)]
    chip: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum, PartialEq)]
enum Volt {
    Auto,
    #[value(name = "1.8")]
    V18,
    #[value(name = "2.5")]
    V25,
    #[value(name = "3.3")]
    V33,
}

impl Volt {
    fn get(self) -> Option<Voltage> {
        match self {
            Volt::Auto => None,
            Volt::V18 => Some(Voltage::V1_8),
            Volt::V25 => Some(Voltage::V2_5),
            Volt::V33 => Some(Voltage::V3_3),
        }
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Show the connected programmer.
    Info,
    /// Check that every chip pin touches the socket.
    Contact,
    /// Identify the chip in the socket.
    Detect,
    /// Read the whole chip into a file.
    Read { file: PathBuf },
    /// Erase, program and verify the chip from a file.
    Write {
        file: PathBuf,
        /// Do not verify after programming.
        #[arg(long)]
        no_verify: bool,
        /// Do not erase before programming (chip must already be blank).
        #[arg(long)]
        no_erase: bool,
    },
    /// Compare the chip with a file.
    Verify { file: PathBuf },
    /// Erase the whole chip.
    Erase,
    /// Check whether the chip is blank (all 0xFF).
    Blank,
    /// List supported chips (optionally filtered).
    Chips {
        filter: Option<String>,
        /// Also list parts the native driver does not handle yet.
        #[arg(long)]
        all: bool,
        /// Show how many parts of each family are supported.
        #[arg(long)]
        stats: bool,
    },
}

fn bar(msg: &str) -> ProgressBar {
    let b = ProgressBar::new(1);
    b.set_style(
        ProgressStyle::with_template(
            "{msg:10} [{bar:40}] {percent:>3}% {bytes}/{total_bytes} {binary_bytes_per_sec}",
        )
        .unwrap()
        .progress_chars("=> "),
    );
    b.set_message(msg.to_string());
    b
}

fn progress(b: &ProgressBar) -> impl FnMut(u64, u64) -> bool + '_ {
    move |done, total| {
        b.set_length(total);
        b.set_position(done);
        true
    }
}

fn target(cli: &Cli) -> Result<Option<Driver>> {
    let Some(name) = &cli.chip else {
        return Ok(None);
    };
    let c = vendor_db::find(name)
        .ok_or_else(|| Error::Chip(format!("Unknown part {name}; see `rt809h chips {name}`")))?;
    c.driver().map(Some).ok_or_else(|| {
        Error::Chip(format!(
            "{} ({}) is not supported by the native driver yet",
            c.name,
            c.family.label()
        ))
    })
}

fn prepare(cli: &Cli) -> Result<(Programmer, DetectedChip)> {
    let mut p = Programmer::connect_with_log(Box::new(|m| eprintln!("{m}")))?;
    p.spi_hz = cli.mhz.clamp(1, 30) * 1_000_000;
    if !cli.no_contact_check {
        let v = cli.voltage.get().unwrap_or(Voltage::V1_8);
        if let Some(problem) = p.check_contact_8pin(v)? {
            return Err(Error::Chip(problem));
        }
        eprintln!("Contact check OK");
    }
    let chip = match target(cli)? {
        Some(Driver::Spi(c)) => p.detect_as(&c)?,
        _ => p.detect(cli.voltage.get())?,
    };
    Ok((p, chip))
}

/// Commands for parts without an ID: I2C/SPI EEPROMs, SPI NAND and the
/// families that run on a logic image.
fn run_part(cli: &Cli, d: &Driver) -> Result<()> {
    let mut p = Programmer::connect_with_log(Box::new(|m| eprintln!("{m}")))?;
    p.spi_hz = cli.mhz.clamp(1, 30) * 1_000_000;
    let eight_pin = !matches!(d, Driver::Image(..));
    if eight_pin && !cli.no_contact_check {
        let v = match d {
            Driver::SpiNand(n) => n.voltage,
            _ => Voltage::V3_3,
        };
        if let Some(problem) = p.check_contact_8pin(v)? {
            return Err(Error::Chip(problem));
        }
        eprintln!("Contact check OK");
    }
    let read = |p: &mut Programmer, msg: &str| -> Result<Vec<u8>> {
        let b = bar(msg);
        let r = p.part_read(d, &mut progress(&b))?;
        b.finish();
        Ok(r)
    };
    let mut note = |m: String| eprintln!("{m}");
    match &cli.cmd {
        Cmd::Detect => println!("{}", p.part_check(d)?),
        Cmd::Read { file } => {
            let data = read(&mut p, "Reading")?;
            std::fs::write(file, &data)?;
            println!("Saved {} bytes to {}", data.len(), file.display());
        }
        Cmd::Write {
            file, no_verify, ..
        } => {
            let data = std::fs::read(file)?;
            let b = bar("Writing");
            p.part_write(d, &data, &mut progress(&b), &mut note)?;
            b.finish();
            if !no_verify {
                let back = read(&mut p, "Verifying")?;
                if let Some(off) = first_difference(&data, &back[..data.len()]) {
                    return Err(Error::Chip(format!("Verify failed at 0x{off:X}")));
                }
            }
            println!("Done");
        }
        Cmd::Verify { file } => {
            let data = std::fs::read(file)?;
            let back = read(&mut p, "Verifying")?;
            let n = data.len().min(back.len());
            match first_difference(&data[..n], &back[..n]) {
                None => println!("Verify OK"),
                Some(off) => return Err(Error::Chip(format!("Mismatch at 0x{off:X}"))),
            }
        }
        Cmd::Erase => {
            let b = bar("Erasing");
            p.part_erase(d, &mut progress(&b), &mut note)?;
            b.finish();
            println!("Erased");
        }
        Cmd::Blank => match read(&mut p, "Reading")?.iter().position(|&x| x != 0xFF) {
            None => println!("Chip is blank"),
            Some(off) => return Err(Error::Chip(format!("Not blank at 0x{off:X}"))),
        },
        _ => unreachable!(),
    }
    Ok(())
}

fn run(cli: Cli) -> Result<()> {
    if !matches!(cli.cmd, Cmd::Chips { .. } | Cmd::Info | Cmd::Contact) {
        match target(&cli)? {
            Some(Driver::Spi(_)) | None => {}
            Some(d) => return run_part(&cli, &d),
        }
    }
    match &cli.cmd {
        Cmd::Chips { filter, all, stats } => {
            if *stats {
                let (mut sup, mut tot) = (0, 0);
                for (f, s, t) in vendor_db::coverage() {
                    println!(
                        "{:<20} {:>6} / {:<6} {:>5.1}%",
                        f.label(),
                        s,
                        t,
                        100.0 * s as f64 / t as f64
                    );
                    sup += s;
                    tot += t;
                }
                println!(
                    "{:<20} {:>6} / {:<6} {:>5.1}%",
                    "Total",
                    sup,
                    tot,
                    100.0 * sup as f64 / tot as f64
                );
                return Ok(());
            }
            let q = filter.as_deref().unwrap_or("");
            for c in vendor_db::search(q)
                .into_iter()
                .filter(|c| *all || c.supported())
            {
                let size = c.driver().map(|d| d.size()).unwrap_or(c.size);
                println!(
                    "{:<20} {:<28} {:<18} {:>9}{}",
                    c.maker,
                    c.name,
                    c.family.label(),
                    if size >= 1024 {
                        format!("{} KiB", size / 1024)
                    } else {
                        format!("{size} B")
                    },
                    if c.supported() {
                        ""
                    } else {
                        "  (not supported yet)"
                    }
                );
            }
        }
        Cmd::Info => {
            let mut p = Programmer::connect()?;
            println!("Programmer: {}", p.info.label);
            println!("Logic:      {}", p.info.algorithm);
            let s = p.supply_raw()?;
            println!("Supply ADC: {} {} {}", s[0], s[1], s[2]);
        }
        Cmd::Contact => {
            let mut p = Programmer::connect()?;
            let v = cli.voltage.get().unwrap_or(Voltage::V1_8);
            let r = p.contact_test(v)?;
            println!("Pins with contact: {:?}", r.connected_pins());
            match p.check_contact_8pin(v)? {
                Some(problem) => return Err(Error::Chip(problem)),
                None => println!("8-pin chip: contact OK"),
            }
        }
        Cmd::Detect => {
            let (_, c) = prepare(&cli)?;
            println!("Chip:    {}", c.name());
            println!("JEDEC:   {}", c.id);
            println!(
                "Size:    {} bytes ({} KiB)",
                c.geometry.size,
                c.geometry.size / 1024
            );
            println!("Voltage: {}", c.voltage);
        }
        Cmd::Read { file } => {
            let (mut p, c) = prepare(&cli)?;
            let b = bar("Reading");
            let data = p.with_flash(&c, |f| f.read(0, c.geometry.size, &mut progress(&b)))?;
            b.finish();
            std::fs::write(file, &data)?;
            println!("Saved {} bytes to {}", data.len(), file.display());
        }
        Cmd::Write {
            file,
            no_verify,
            no_erase,
        } => {
            let data = std::fs::read(file)?;
            let (mut p, c) = prepare(&cli)?;
            if data.len() as u64 > c.geometry.size as u64 {
                return Err(Error::Chip(format!(
                    "File is {} bytes but the chip holds {}",
                    data.len(),
                    c.geometry.size
                )));
            }
            p.with_flash(&c, |f| {
                if !no_erase {
                    let b = bar("Erasing");
                    if data.len() as u32 == c.geometry.size {
                        f.erase_chip(&mut progress(&b))?;
                    } else {
                        f.erase_range(0, data.len() as u32, &mut progress(&b))?;
                    }
                    b.finish();
                }
                let b = bar("Writing");
                f.program(0, &data, &mut progress(&b))?;
                b.finish();
                if !no_verify {
                    let b = bar("Verifying");
                    let back = f.read(0, data.len() as u32, &mut progress(&b))?;
                    b.finish();
                    if let Some(off) = first_difference(&data, &back) {
                        return Err(Error::Chip(format!("Verify failed at 0x{off:X}")));
                    }
                }
                Ok(())
            })?;
            println!("Done");
        }
        Cmd::Verify { file } => {
            let data = std::fs::read(file)?;
            let (mut p, c) = prepare(&cli)?;
            let len = (data.len() as u32).min(c.geometry.size);
            let b = bar("Verifying");
            let back = p.with_flash(&c, |f| f.read(0, len, &mut progress(&b)))?;
            b.finish();
            match first_difference(&data, &back) {
                None => println!("Verify OK"),
                Some(off) => return Err(Error::Chip(format!("Mismatch at 0x{off:X}"))),
            }
        }
        Cmd::Erase => {
            let (mut p, c) = prepare(&cli)?;
            let b = bar("Erasing");
            p.with_flash(&c, |f| f.erase_chip(&mut progress(&b)))?;
            b.finish();
            println!("Erased");
        }
        Cmd::Blank => {
            let (mut p, c) = prepare(&cli)?;
            let b = bar("Reading");
            let data = p.with_flash(&c, |f| f.read(0, c.geometry.size, &mut progress(&b)))?;
            b.finish();
            match data.iter().position(|&x| x != 0xFF) {
                None => println!("Chip is blank"),
                Some(off) => return Err(Error::Chip(format!("Not blank at 0x{off:X}"))),
            }
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {e}");
            if matches!(e, Error::Usb(_) | Error::NotFound) {
                eprintln!("Is the programmer plugged in and the udev rule installed?");
            }
            ExitCode::FAILURE
        }
    }
}
