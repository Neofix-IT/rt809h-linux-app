# Changelog

## [Unreleased]

### Added
- Full part list: pick any chip by name in the GUI or with `--chip`
- 24xx I²C EEPROMs, 25xx/95xx SPI EEPROMs and SPI NAND drivers
- 93xx Microwire, parallel NOR/EEPROM and EPROM via programmer logic images
- AVR microcontrollers over the ISP header: flash, EEPROM and fuses
- AVR over UPDI (tinyAVR 0/1/2, megaAVR 0, AVR Dx/Ex) and a GUI fuse editor
- ENE KB9012 embedded controllers over EDI (untested on hardware)
- Connection diagrams: socket placement and in-circuit wiring per part

### Fixed
- Long MPSSE transfers no longer stall the USB link

### Changed
- Redesigned GUI: chip sidebar, operation tiles, progress with time left,
  confirmation before erase and write, coloured hex view with go-to-address,
  drag and drop, keyboard shortcuts (Ctrl+O, Ctrl+S, Ctrl+R, F5), light and
  dark theme

## [0.1.0] - 2026-11-21

### Added
- Pure Rust FT2232H driver and RT809H MCU protocol
- Socket contact check with orientation detection
- Chip auto-detection with 1.8 → 2.5 → 3.3 V ramping, SFDP fallback
- Read, write, verify, erase and blank check for 25-series SPI NOR flash
- GUI (`rt809h-gui`) and CLI (`rt809h`)
- Debian and Arch packages, udev rule, release workflow
