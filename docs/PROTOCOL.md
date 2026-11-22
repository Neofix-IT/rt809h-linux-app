# RT809H protocol notes

Facts about the device interface as observed on the USB bus. Everything here
was derived from traffic captures and experiments with the hardware.

## USB

FT2232H, VID:PID `0403:6010`, manufacturer string `RETOOLS`. Only channel A
(interface 0, EP `0x02` out / `0x81` in) is used. Channel A is configured for
FIFO mode in the FTDI EEPROM; the host temporarily switches it to MPSSE.

Init: `SIO_RESET`, latency timer 2 ms, RTS/CTS flow control, purge.

## MCU command frames

Before every frame the host resynchronises the MCU:

```
SETBITMODE 0xFF, MPSSE
82 00 40          ACBUS6 = 0 (output)
  (~3 ms)
82 FF 00          release ACBUS
SETBITMODE 0xFF, RESET (back to FIFO)
  (~50 ms settle)
```

Frame (10 bytes): `AA cmd p1 p2 chk t0 t1 t2 t3 t4`

- `chk = rotl8(XOR of the other nine bytes, 3)`
- The MCU echoes the frame with `AA` replaced by `55`.
- On a bad check byte it answers with the expected check byte instead of `55`
  and does not execute the command.
- Unused parameter bytes are `CD`.

| cmd | p1 p2 | tail | data | meaning |
|-----|-------|------|------|---------|
| `EA` | `CD CD` | `CD…` | ← 256 | loaded logic image, byte 1.. = name (`BASE` after power-up) |
| `A5` | `AA 55` | `00 CD…` | | return to the `BASE` image, then `82 FF 40` and reset |
| `15` | `CD CD` | `CD…` | → 128, ← 1 | session handshake (random challenge) |
| `F5` | `CD CD` | `CD…` | ← 64 | hardware info / serial |
| `56` | `14 00` | `CD…` | → 40, ← 40 | rail configuration, 20 × u16 LE |
| `58` | `03 00` | `CD…` | ← 6 | supply monitor, 3 × u16 LE (bit 15 flag, 10-bit ADC) |
| `3A` | `00 00` | `00 00 00 00 14` | → 12 zero, ← 48 | socket contact scan |
| `52` | `00 C0` | `01 01 01 01 01` | | hand the socket to the MPSSE SPI engine; mode byte needs bit 7 (`80`/`C0`), otherwise the SPI lines stay disconnected and read `00` |
| `01` | `00 C0` | `01 01 01 01 01` | | power off after `52` (no echo is read), then ACBUS7 pulse `82 00 80` / `82 FF 00` |
| `01` | `00 00` | `00 00 00 00 14` | | power off after a contact scan without `52` |
| `5A` | `55 AA` | `CD CD CD CD n` | → n × 32 KiB | load a logic image (not needed for SPI) |

### Rail configuration (`56`)

| word | value |
|------|-------|
| 0 | `0x0010` |
| 1 | level code: `0xC2` @ 3.3 V, `0xE0` @ 1.8 V (`0xD2` used for 2.5 V) |
| 2 | `0x00EA` |
| 3–14, 16, 17 | `0x0211` |
| 15 | `0x0294` |
| 18 | VCC in 5 mV steps (`0x0294` = 3.3 V, `0x01F4` = 2.5 V, `0x0168` = 1.8 V) |
| 19 | VCCIO in 5 mV steps |

### Contact scan (`3A`)

Needs the rails switched on with `56` first. 48 bytes back, after ~160 ms:

- bytes 8–15: little-endian contact map of the bottom eight socket rows,
  a **0** bit means a chip pin was detected:
  bit *k* (0–7) = ZIF pin 25+*k* (right column, bottom up),
  bit 8+*k* = ZIF pin 17+*k* (left column, top down).
- bytes 16–29: status and pin lists, not decoded. `ff ff 01 1c …` is
  reported for a correctly seated chip.
- bytes 30–39: echo of rail configuration words 15–19.

ZIF numbering: pins 1–24 down the left column, 25–48 up the right column.
An 8-pin chip pushed to the bottom with pin 1 at the top occupies 21–24
(pins 1–4) and 25–28 (pins 5–8). Anything else (a missing pin or a chip one
row higher) makes SPI read `FF`.

## SPI through MPSSE

Session order: `56` (rails) → `3A` (contact) → `52 00 C0`, then
`SETBITMODE 0xFF RESET`, `SETBITMODE 0xFF MPSSE`.
ADBUS0 = SCK, ADBUS1 = MOSI, ADBUS2 = MISO, ADBUS3 = CS#, direction `0x0B`.
SPI mode 0 (`0x11` out on falling edge, `0x20` in on rising edge), 60 MHz base
clock with `0x8A`. Standard 25-series opcodes from there on.

## I2C EEPROMs (24xx)

Runs on the power-up `BASE` logic. Rails as for SPI but level code `0xC0`
(3.3 V). Routing: `52 00 00` tail `01 00 00 00 00`; power off with `01 00 00`
tail `01 00 00 00 00`, then the ACBUS7 pulse. (Captured from host software
under Wine, which is known to clear the connect bits for SPI; the driver
falls back to `80`/`C0` if nothing acknowledges.)

The bus is bit-banged with MPSSE GPIO writes, 100 kHz (`86 3B 00`):
ADBUS0 = SCL, ADBUS1 = SDA out, ADBUS2 = SDA in, direction `0x13` while
driving SDA, `0x11` to release it, `0x10` when idle.

- START: `80 03 13` ×4, `80 01 13` ×8, `80 00 13` ×4
- byte out: `11 00 00 bb`, ACK read: `80 02 11` ×3, `22 00`, `87`
- byte in: `80 02 13` ×2, `24 00 00`, then `80 00 13` (ACK) or
  `80 02 13` (NACK) and `8E 00`
- STOP: `80 02 13`, `80 00 13`, `80 01 13` ×4, `80 03 13` ×8, `80 00 10`

Bit-mode reads keep shifting into the same register, so a 1-bit read
returns the previous bits above bit 0. Writes use the chip's page size and
acknowledge polling.

## Logic images

Every other family needs a logic image in the programmer. The host uploads
it when the chip is selected:

```
A5 AA 55  tail 00 CD CD CD CD       back to BASE first (if another image is loaded)
5A 55 AA  tail CD CD CD CD n        then n blocks of 32 KiB, each answered by 1 byte
EA                                  → 256 bytes, byte 0 = 1, name from byte 1 ("ALGO_NOR_8_1")
```

The blocks are the image file as distributed with the vendor software
(`LIB/ALGO_*.DAT`), padded with `FF` to a multiple of 32 KiB. No image is
included in this project.

With an image loaded every frame uses the tail `EE 00 00 00 1E` and the
chip operations are executed by the MCU:

| cmd | data | meaning |
|-----|------|---------|
| `56` | → 40, ← 40 | rails (as above, level code e.g. `0x9E` for 93xx, `0xA8` for parallel NOR) |
| `3A` | → 12, ← 48 | contact scan |
| `52` | → 256, ← 1 | chip parameters (see below) |
| `61` | ← 256 | read ID (parallel flash: byte 0/1 = maker/device) |
| `65` | ← size | read the chip (64 KiB transfers) |
| `6A` | ← size | read again (verify pass) |
| `7A` | → size, ← `AA 55` | program the chip |
| `85` | ← 1 (`AA`) | erase the whole chip; the answer comes when the erase finishes or the part's erase timeout (units of 10 ms) runs out |

Parameter block (little endian), first 40 bytes; the rest is zero in the
captures. The rail words repeat the `56` table; the other fields come from
the part record or are fixed per logic image.

| offset | source | 93C46 | SST39SF040 | 28F320 (16-bit) | EN29GL064B (8-bit) |
|--------|--------|-------|------------|-----------------|--------------------|
| 0 u32 | capacity | `0x80` | `0x80000` | `0x200000` | `0x800000` |
| 6 u16 | rail word 1 (logic level) | `0x9E` | `0xA8` | `0xC4` | `0xC4` |
| 8 u16 | rail word 2 | `0x212` | `0x212` | `0x8D` | `0x212` |
| 10 u16 | rail word 3 | `0x211` | `0x211` | `0x211` | `0x211` |
| 12 u16 | rail word 15 (Vpp) | `0x294` | `0x294` | `0x294` | `0x294` |
| 14 u16 | rail word 18 (Vcc) | `0x294` | `0x258` | `0x294` | `0x294` |
| 16 u32 | erase timeout | 0 | 50000 | 65000 | 0 |
| 20 u32 | program timeout | 0 | 500 | 1000 | 200 |
| 24 u32 | write buffer | 8 | 8 | 8 | 32 |
| 28 u16 | page | 0x100 | 0x100 | 0x100 | 0x100 |
| 30 u16 | image flag | 0 | 0 | 0 | 1 |
| 32 u32 | fixed | 0x100000 | 0x100000 | 0x100000 | 0x100000 |
| 36 u32 | image flag | 1 | 1 | 0x80000001 | 1 |

The per-image values (rail words 1/2/15/18 and the two flags) are kept in
`image::PROFILES`; capacity, timeouts, buffer and page come from the part
list.

After an operation the host pulses ACBUS7 (`82 00 80`) and resets the FTDI.

## Part list

`crates/rt809h-core/data/vendor_chips.tsv` lists every part the vendor
software offers, with its family, capacity, JEDEC ID (SPI) and the logic
image / algorithm it uses, plus the erase/program timeouts and the
buffer/page fields of the part record (`t_erase`, `t_program`, `buffer`,
`page`). Families by part count: SPI flash, I2C EEPROM, parallel NOR,
Microwire, MCUs, EPROM, eMMC, NAND, parallel EEPROM, PLD.

Rail word 1 (logic level) is set per part by the vendor software, roughly
`3.3 V - (code - 0xC2) * 0.05 V` (`0x9E` ≈ 5.1 V, `0xA8` ≈ 4.6 V, `0xC8` ≈ 3.0 V,
`0xDC` ≈ 2.0 V). The part list does not carry it, so image profiles use the
lowest level captured for that image. Parallel NAND additionally needs the
page count (offset 0), page size and pages per block (u16 at 24 and 26) and
further geometry bytes from offset 52; it is not implemented.
