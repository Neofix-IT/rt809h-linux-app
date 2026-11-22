# RT809H for Linux

A simple desktop app (and a CLI) for the **RT809H** universal programmer on
Linux. No Wine, no Windows software, no libusb: install it, plug in the
programmer and click **Detect**, **Read** or **Write**.

> **Unofficial project.** Not made, endorsed or supported by the manufacturer
> of the RT809H. Use at your own risk.

## The app

Start **RT809H Programmer** from your application menu (or run `rt809h-gui`).

- Finds the chip by itself, ramping the voltage safely from 1.8 V to 3.3 V
- Checks the socket contact first and tells you about loose pins or a chip
  that sits too high
- Shows where the chip goes in the ZIF socket and how to wire in-circuit
  parts
- Read, write with verify, erase, blank check, and a hex preview of the data

## Supported chips

| Family | Status |
|--------|--------|
| 25-series SPI NOR flash | tested |
| AVR over ISP (ATmega, ATtiny, AT90USB) | tested (ATmega328P) |
| AVR over UPDI (tinyAVR 0/1/2, megaAVR 0, AVR DA/DB/DD/DU/EA/EB) | tested (ATtiny212), other families untested |
| 24xx I²C EEPROMs | tested on an emulated chip |
| 93xx Microwire, parallel NOR/EEPROM, EPROM (read) | experimental¹ |
| SPI NAND, 25xx/95xx SPI EEPROMs | untested |
| NAND, eMMC, other MCUs, PLDs | not supported |

¹ These need the programmer logic files (`LIB/ALGO_*.DAT`) from the vendor's
Windows software: copy its `LIB` folder to `~/.local/share/rt809h/algos`. An
existing Wine install is found automatically.

Chips without an ID (EEPROMs, SPI NAND, parallel chips, MCUs) are picked
from the chip list. The RT809H has no UPDI port, so the app generates UPDI
on the socket. The Connection panel shows the wiring: one 1 kΩ resistor, and
the target runs from its own 3.3 V supply. Check AVR fuse values against the
datasheet before writing them, since wrong ones can lock the part out.

## Install

Get the package from the
[releases page](https://github.com/Neofix-IT/rt809h-linux-app/releases):

```bash
sudo apt install ./rt809h_*_amd64.deb      # Debian / Ubuntu
```

```bash
sudo pacman -U rt809h-*.pkg.tar.zst        # Arch / Manjaro / EndeavourOS
```

Re-plug the RT809H once afterwards. A portable tarball is there too: copy
its `60-rt809h.rules` to `/etc/udev/rules.d/`, then run `./rt809h-gui`.

## Command line

```bash
rt809h detect
rt809h read dump.bin
rt809h write firmware.bin                 # erase + program + verify
rt809h --chip AT24C02 read eeprom.bin     # parts without an ID
rt809h fuses --updi                       # identify an AVR over UPDI
rt809h -c ATmega328P fuses --low 0xFF --high 0xDE
```

`rt809h --help` lists every command and option.

## Development

```bash
cargo build --release && ./target/release/rt809h-gui
scripts/release.sh 0.2.0 --push           # tag a release; CI builds the packages
```

The RT809H is an FT2232H plus a microcontroller. The app drives both directly
over USB; the protocol, worked out from USB traffic, is in
[docs/PROTOCOL.md](docs/PROTOCOL.md).

## License

MIT
