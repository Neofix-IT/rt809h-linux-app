//! Core library for the RT809H universal programmer.

pub mod avr;
pub mod chips;
pub mod emu;
pub mod ftdi;
pub mod i2c;
pub mod image;
pub mod protocol;
pub mod session;
pub mod spi;
pub mod spi_eeprom;
pub mod spi_nand;
pub mod vendor_db;

pub use protocol::Voltage;
pub use session::{DetectedChip, Programmer};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("USB error: {0}")]
    Usb(String),
    #[error("timeout: {0}")]
    Timeout(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("no RT809H programmer found")]
    NotFound,
    #[error("{0}")]
    Chip(String),
    #[error("cancelled")]
    Cancelled,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl Error {
    pub(crate) fn usb(e: impl std::fmt::Display) -> Self {
        Error::Usb(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
