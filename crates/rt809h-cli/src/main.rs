use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};
use rt809h_core::chips;
use rt809h_core::session::first_difference;
use rt809h_core::{DetectedChip, Error, Programmer, Result, Voltage};

#[derive(Parser)]
#[command(
    name = "rt809h",
    version,
    about = "RT809H programmer for 25-series SPI flash"
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
    Chips { filter: Option<String> },
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
    let chip = p.detect(cli.voltage.get())?;
    Ok((p, chip))
}

fn run(cli: Cli) -> Result<()> {
    match &cli.cmd {
        Cmd::Chips { filter } => {
            for c in chips::search(filter.as_deref().unwrap_or("")) {
                println!(
                    "{:<12} {:<22} {:06X} {:>7} KiB  {}",
                    c.vendor,
                    c.name,
                    c.id,
                    c.size / 1024,
                    c.voltage
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
