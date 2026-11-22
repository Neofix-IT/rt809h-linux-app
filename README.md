# RT809H for Linux

Native Linux app and command line tool for the **RT809H** universal programmer.
No Wine, no Windows software, no proprietary libraries — the programmer is
driven directly over USB (pure Rust, no libusb/libftdi needed).

> **Unofficial project.** This app is not made, endorsed or supported by the
> manufacturer of the RT809H. Use it at your own risk; for official support
> use the vendor's Windows software.

## Features

- Automatic chip detection with **safe voltage ramping** (1.8 V → 2.5 V → 3.3 V);
  the chip is then driven at its rated voltage from the built-in database
- Manual voltage selection (1.8 / 2.5 / 3.3 V) and manual chip selection
- **Socket contact check** before every operation: reports loose pins, an empty
  socket and a chip placed too high
- Read, write (erase + program + verify), verify, erase, blank check
- Unknown chips are handled through SFDP / JEDEC capacity
- Simple English GUI with hex preview, plus a scriptable CLI

## What is supported

`rt809h chips --stats` prints the current coverage of the full part list.

| Family | How | Status |
|--------|-----|--------|
| 25-series SPI NOR flash | native SPI | tested |
| 24xx I²C EEPROMs | native bit-banged I²C | tested against an emulated chip, needs a real one |
| 93xx Microwire EEPROMs | programmer logic image | read flow tested (empty socket) |
| SPI NAND (W25N, MX35, GD5F, …) | native SPI | untested, needs a chip |
| 25xx/95xx SPI EEPROMs | native SPI | untested, needs a chip |
| Parallel NOR, parallel EEPROM | programmer logic image | experimental |
| EPROM (27Cxxx) | programmer logic image | experimental, read only |
| AVR microcontrollers (ATmega, ATtiny, AT90USB) | native ISP | tested (ATmega328P) |
| NAND, eMMC, other MCUs, PLDs | – | not supported |

Parts without an ID (EEPROMs, SPI NAND, parallel chips) must be named with
`--chip`, e.g. `rt809h --chip AT24C02 read dump.bin`.

The families marked "logic image" need the programmer logic files
(`LIB/ALGO_*.DAT`) from the vendor's Windows software, which are not part of
this project. Copy that `LIB` folder to `~/.local/share/rt809h/algos` (or set
`RT809H_ALGO_DIR`); an existing Wine installation is found automatically.
"Experimental" means the supply settings were taken from a representative
part of the same family; check the result with a verify before relying on it.

### AVR over ISP

Connect the target to the RT809H ISP header (MOSI, MISO, SCK, RESET, GND
and VCC); the target board supplies its own power. The ISP clock is found
automatically.

```bash
rt809h fuses                                       # identify the part, show fuses
rt809h -c ATmega328P read flash.bin
rt809h -c ATmega328P write flash.bin               # chip erase + program + verify
rt809h -c ATmega328P --memory eeprom read ee.bin
rt809h -c ATmega328P fuses --low 0xFF --high 0xDE  # program fuses
```

A flash write starts with a chip erase, which also clears the EEPROM unless
the EESAVE fuse is set. Wrong fuse values (clock source, RSTDISBL, SPIEN) can
lock the part out of ISP, so check them against the datasheet first.

## Install

Download the package for your distribution from the
[releases page](https://github.com/Neofix-IT/rt809h-linux-app/releases):

```bash
sudo apt install ./rt809h_*_amd64.deb      # Debian / Ubuntu
```

```bash
sudo pacman -U rt809h-*.pkg.tar.zst        # Arch / Manjaro / EndeavourOS
```

Both packages install a udev rule that gives your user access to the
programmer. Re-plug the RT809H once after installing.

### Portable tarball

```bash
tar xf rt809h-*-linux-x86_64.tar.gz && cd rt809h-*-linux-x86_64
sudo cp 60-rt809h.rules /etc/udev/rules.d/ && sudo udevadm control --reload && sudo udevadm trigger
./rt809h-gui
```

## Using it

Put the chip at the **bottom** of the ZIF socket (lever at the top left),
**notch down with pin 1 (dot) on the bottom-right contact**. The app's
Connection panel draws where the selected part goes, which adapter it needs
and how to wire it in circuit. Then press **Detect**, **Read** or
**Write** in the app, or use the CLI:

```bash
rt809h detect
rt809h read dump.bin
rt809h write firmware.bin
rt809h verify firmware.bin
rt809h erase
rt809h --voltage 1.8 detect      # force a supply voltage
rt809h --chip 93C46 read eeprom.bin   # parts without an ID need --chip
rt809h contact                   # contact test only
```

## Building from source

```bash
cargo build --release
./target/release/rt809h-gui
```

Runtime libraries for the GUI: `libxkbcommon-x11`, `libGL`/`libEGL`
(present on any desktop install).

## Releasing

```bash
scripts/release.sh 0.2.0 --push
```

bumps the version, moves the `Unreleased` changelog entries under the new
version, commits, tags and pushes. The `Release` GitHub workflow then builds
the `.deb`, the Arch package and a portable tarball and publishes them on a
GitHub release.

## How it works

The RT809H is an FT2232H plus a microcontroller. The app talks to the MCU
through the FTDI FIFO interface (power rails, contact scan, pin routing) and
then clocks SPI through the FTDI MPSSE engine. The command set is documented
in [docs/PROTOCOL.md](docs/PROTOCOL.md). It was worked out by observing the
USB traffic of the programmer; no vendor code is included or required.

## License

MIT
