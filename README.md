# RT809H for Linux

Native Linux app and command line tool for the **RT809H** universal programmer.
No Wine, no Windows software, no proprietary libraries — the programmer is
driven directly over USB (pure Rust, no libusb/libftdi needed).

![status](https://img.shields.io/badge/status-early-orange)

## Features

- Automatic chip detection with **safe voltage ramping** (1.8 V → 2.5 V → 3.3 V);
  the chip is then driven at its rated voltage from the built-in database
- Manual voltage selection (1.8 / 2.5 / 3.3 V) and manual chip selection
- **Socket contact check** before every operation: reports loose pins, an empty
  socket, a chip placed too high and a chip inserted the wrong way round
- Read, write (erase + program + verify), verify, erase, blank check
- Unknown chips are handled through SFDP / JEDEC capacity
- Simple English GUI with hex preview, plus a scriptable CLI

Currently supported: 25-series SPI NOR flash in the ZIF socket (8-pin, pushed
to the bottom of the socket). See `rt809h chips` for the database.

## Install

Download the package for your distribution from the
[releases page](https://github.com/Neofix-IT/rth809h-linux-app/releases):

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

Put the chip at the **bottom** of the ZIF socket with **pin 1 on the lever
side** (chip pin 1 = socket pin 21). Then press **Detect**, **Read** or
**Write** in the app, or use the CLI:

```bash
rt809h detect
rt809h read dump.bin
rt809h write firmware.bin
rt809h verify firmware.bin
rt809h erase
rt809h --voltage 1.8 detect      # force a supply voltage
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
