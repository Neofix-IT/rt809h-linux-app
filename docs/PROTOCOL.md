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
