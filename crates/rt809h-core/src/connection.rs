//! How a part is connected to the programmer: where it sits in the 48-pin
//! ZIF socket, or which socket contacts are wired to which signal when it is
//! programmed in circuit.
//!
//! Positions are physical: the socket seen from above with the lever at the
//! top left, rows counted from the top (1) to the bottom (24). Parts sit at
//! the bottom of the socket with pin 1 at the bottom right (notch down).

use crate::vendor_db::{Driver, VendorChip};

/// A part placed in the socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    /// pin count of the DIP footprint that goes into the socket
    pub pins: u8,
    /// adapter the part sits on, if any
    pub adapter: Option<String>,
}

/// One socket contact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pad {
    /// right column (else left)
    pub right: bool,
    /// 1 = top (lever end) … 24 = bottom
    pub row: u8,
}

impl Pad {
    pub const fn left(row: u8) -> Self {
        Pad { right: false, row }
    }

    pub const fn right(row: u8) -> Self {
        Pad { right: true, row }
    }

    /// e.g. "right column, 1st from the bottom"
    pub fn describe(self) -> String {
        let n = 25 - self.row;
        let suffix = match n {
            1 => "st",
            2 => "nd",
            3 => "rd",
            _ => "th",
        };
        format!(
            "{} column, {n}{suffix} from the bottom",
            if self.right { "right" } else { "left" }
        )
    }
}

impl Placement {
    /// Contact under chip (or adapter) pin `n` (1-based): pins 1 … n/2 run
    /// up the right column from the bottom, the rest down the left column.
    pub fn pad(&self, n: u8) -> Pad {
        let half = self.pins / 2;
        if n <= half {
            Pad::right(25 - n)
        } else {
            Pad::left(24 - half + (n - half))
        }
    }
}

/// Flying leads from socket pins to a part on a board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wiring {
    /// what sits at the other end of the wires
    pub target: &'static str,
    /// (socket contact, signal)
    pub wires: Vec<(Pad, &'static str)>,
    pub notes: Vec<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Diagram {
    Socket(Placement),
    Wires(Wiring),
    /// No drawing; the text says what to do.
    Text(String),
}

impl Diagram {
    pub fn title(&self) -> &'static str {
        match self {
            Diagram::Socket(_) | Diagram::Text(_) => "In the socket",
            Diagram::Wires(_) => "In circuit",
        }
    }
}

const EIGHT: Placement = Placement {
    pins: 8,
    adapter: None,
};

const SHORT: &str = "Keep the wires shorter than 10 cm.";

fn eight_pin(signals: [&'static str; 8], target: &'static str, used: &[u8]) -> Wiring {
    Wiring {
        target,
        wires: (1..=8u8)
            .filter(|n| used.contains(n))
            .map(|n| (EIGHT.pad(n), signals[n as usize - 1]))
            .collect(),
        notes: vec![
            SHORT,
            "Each chip pin goes to the contact it would use if the chip sat in the socket.",
        ],
    }
}

/// 25-series SPI flash / EEPROM in circuit.
pub fn spi_in_circuit() -> Wiring {
    eight_pin(
        [
            "CS# (1)",
            "SO (2)",
            "WP# (3)",
            "VSS (4)",
            "SI (5)",
            "SCLK (6)",
            "HOLD# (7)",
            "VCC (8)",
        ],
        "25xx chip",
        &[1, 2, 3, 4, 5, 6, 7, 8],
    )
}

/// 24-series I2C EEPROM in circuit.
pub fn i2c_in_circuit() -> Wiring {
    eight_pin(
        [
            "A0 (1)", "A1 (2)", "A2 (3)", "GND (4)", "SDA (5)", "SCL (6)", "WP (7)", "VCC (8)",
        ],
        "24xx chip",
        &[4, 5, 6, 7, 8],
    )
}

/// 93-series Microwire EEPROM in circuit.
pub fn microwire_in_circuit() -> Wiring {
    let mut w = eight_pin(
        [
            "CS (1)", "SK (2)", "DI (3)", "DO (4)", "GND (5)", "ORG (6)", "NC (7)", "VCC (8)",
        ],
        "93xx chip",
        &[1, 2, 3, 4, 5, 8],
    );
    w.notes.push(
        "Some 93xx parts (BR93 series) use a different pinout; check the datasheet before wiring.",
    );
    w
}

/// AVR microcontroller on its ISP pins.
pub fn avr_isp() -> Wiring {
    Wiring {
        target: "AVR board",
        wires: vec![
            (Pad::left(21), "RESET"),
            (Pad::left(22), "MISO"),
            (Pad::left(24), "GND"),
            (Pad::right(24), "MOSI"),
            (Pad::right(23), "SCK"),
        ],
        notes: vec![
            SHORT,
            "Power the board from its own supply; VCC is not connected to the programmer.",
        ],
    }
}

/// Package suffix of a part name (`W25Q64@SOP8` → `SOP8`).
fn package(name: &str) -> Option<&str> {
    let p = name.rsplit_once('@')?.1;
    let p = p.split(['(', ' ', '-']).next().unwrap_or(p);
    (!p.is_empty()).then_some(p)
}

fn trailing_number(s: &str) -> Option<u8> {
    let digits: String = s
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    digits.parse().ok()
}

/// SOP16 25-series flash: its pins wired to the 8-pin positions.
pub fn spi_sop16() -> Wiring {
    Wiring {
        target: "SOP16 flash",
        wires: vec![
            (EIGHT.pad(1), "CS# (pin 7)"),
            (EIGHT.pad(2), "SO (pin 8)"),
            (EIGHT.pad(3), "WP# (pin 9)"),
            (EIGHT.pad(4), "GND (pin 10)"),
            (EIGHT.pad(5), "SI (pin 15)"),
            (EIGHT.pad(6), "SCLK (pin 16)"),
            (EIGHT.pad(7), "HOLD# (pin 1)"),
            (EIGHT.pad(8), "VCC (pin 2)"),
        ],
        notes: vec![
            "The 16-pin package does not fit the 8-pin positions directly: wire an SOP16 \
             adapter (or the chip) like this.",
        ],
    }
}

/// `eight`: the family is 8-pin, so a part without a package goes in as DIP8.
fn placement(name: &str, eight: bool) -> Diagram {
    let full = name
        .rsplit_once('@')
        .map_or("", |p| p.1)
        .to_ascii_uppercase();
    let Some(pkg) = package(name).map(str::to_ascii_uppercase) else {
        return if eight {
            Diagram::Socket(Placement {
                pins: 8,
                adapter: Some("SOP8/WSON8 to DIP8 adapter for SMD parts".into()),
            })
        } else {
            Diagram::Text(
                "Put a DIP part at the bottom of the socket with pin 1 at the bottom right \
                 (notch down); other packages need the matching adapter, placed the same way."
                    .into(),
            )
        };
    };
    let n = trailing_number(&pkg);
    let socket = |pins: u8, adapter: Option<String>| Diagram::Socket(Placement { pins, adapter });
    match n {
        Some(n) if pkg.starts_with("DIP") || pkg.starts_with("PDIP") => socket(n, None),
        Some(8) => socket(8, Some(format!("{pkg} to DIP8 adapter"))),
        Some(n @ (14 | 16 | 20 | 24 | 28)) if pkg.starts_with("SO") => {
            socket(n, Some(format!("{pkg} to DIP{n} adapter")))
        }
        Some(32) if pkg.starts_with("PLCC") => socket(32, Some("PLCC32 to DIP32 adapter".into())),
        Some(44) if pkg.starts_with("PLCC") && full.contains("DIP40") => {
            socket(40, Some("PLCC44 to DIP40 adapter".into()))
        }
        Some(48) if pkg.starts_with("TSOP") => socket(48, Some("TSOP48 adapter".into())),
        _ => Diagram::Text(format!(
            "This part needs a {pkg} adapter. Put the adapter at the bottom of the socket \
             with its pin 1 at the bottom right."
        )),
    }
}

/// Connection diagrams for `chip` (None = auto-detected SPI flash): how it
/// goes into the socket and, where the part can be programmed on its board,
/// how to wire it.
pub fn diagrams(chip: Option<&VendorChip>) -> Vec<Diagram> {
    let Some(c) = chip else {
        return vec![placement("", true), Diagram::Wires(spi_in_circuit())];
    };
    match c.driver() {
        Some(Driver::Avr(..)) => vec![Diagram::Wires(avr_isp())],
        Some(Driver::Spi(_) | Driver::SpiEeprom(_) | Driver::SpiNand(_))
            if package(c.name).is_some_and(|p| trailing_number(p) == Some(16)) =>
        {
            vec![Diagram::Wires(spi_sop16())]
        }
        Some(Driver::Spi(_) | Driver::SpiEeprom(_) | Driver::SpiNand(_)) => {
            vec![placement(c.name, true), Diagram::Wires(spi_in_circuit())]
        }
        Some(Driver::I2c(_)) => vec![placement(c.name, true), Diagram::Wires(i2c_in_circuit())],
        Some(Driver::Image(algo, _)) if algo.starts_with("ALGO_93") => {
            vec![
                placement(c.name, true),
                Diagram::Wires(microwire_in_circuit()),
            ]
        }
        _ => vec![placement(c.name, false)],
    }
}

/// Plain-text form for the command line.
pub fn describe(d: &Diagram) -> String {
    match d {
        Diagram::Text(t) => format!("{}: {t}", d.title()),
        Diagram::Socket(p) => {
            let mut s = format!(
                "{}: {}-pin footprint at the bottom of the socket (lever at the top left), \
                 pin 1 at the bottom right, notch down",
                d.title(),
                p.pins
            );
            if let Some(a) = &p.adapter {
                s.push_str(&format!(". Use a {a}, placed the same way"));
            }
            s
        }
        Diagram::Wires(w) => {
            let mut s = format!(
                "{} (wires from the socket to the {}, lever at the top left):",
                d.title(),
                w.target
            );
            for (pad, sig) in &w.wires {
                s.push_str(&format!("\n  {:<14} <- {}", sig, pad.describe()));
            }
            for n in &w.notes {
                s.push_str(&format!("\n  {n}"));
            }
            s
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vendor_db;

    #[test]
    fn layouts() {
        let p8 = EIGHT;
        assert_eq!(p8.pad(1), Pad::right(24));
        assert_eq!(p8.pad(4), Pad::right(21));
        assert_eq!(p8.pad(5), Pad::left(21));
        assert_eq!(p8.pad(8), Pad::left(24));
        let p32 = Placement {
            pins: 32,
            adapter: None,
        };
        assert_eq!(
            (p32.pad(1), p32.pad(16), p32.pad(17)),
            (Pad::right(24), Pad::right(9), Pad::left(9))
        );
        assert_eq!(
            Pad::right(24).describe(),
            "right column, 1st from the bottom"
        );

        let avr = diagrams(vendor_db::find("ATmega328P"));
        assert_eq!(avr, vec![Diagram::Wires(avr_isp())]);
        let nor = diagrams(vendor_db::find("SST39SF040"));
        assert!(!nor.is_empty());
        let w = spi_in_circuit();
        assert_eq!(w.wires[0], (Pad::right(24), "CS# (1)"));
        assert_eq!(w.wires[7], (Pad::left(24), "VCC (8)"));
        assert!(matches!(
            placement("X@PLCC32", false),
            Diagram::Socket(Placement { pins: 32, .. })
        ));
        assert!(matches!(
            placement("X@TSOP48", false),
            Diagram::Socket(Placement { pins: 48, .. })
        ));
        assert!(matches!(
            placement("X@DIP28", false),
            Diagram::Socket(Placement {
                pins: 28,
                adapter: None
            })
        ));
        assert!(matches!(placement("X@BGA64", false), Diagram::Text(_)));
        assert!(matches!(
            placement("X@SOP8-200", true),
            Diagram::Socket(Placement { pins: 8, .. })
        ));
        assert!(matches!(
            placement("X@", true),
            Diagram::Socket(Placement { pins: 8, .. })
        ));
    }
}
