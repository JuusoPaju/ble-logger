# ESP32-S3 Wi-Fi / BLE Logger

ESP32-S3 Wi-Fi frame / BLE observation logger and host-side extraction tools. The logger keeps
an append-only, CRC-protected record stream in a dedicated flash partition and
uses the ESP32-S3 native USB Serial/JTAG peripheral for control and downloads.

## Host tools

```sh
cargo test
cargo run -p blelog-cli -- --help
cargo run -p blelog-cli -- ports
cargo run -p blelog-cli -- prepare --port /dev/ttyACM0
cargo run -p blelog-cli -- pull --port /dev/ttyACM0 --output walk.blelog
cargo run -p blelog-cli -- verify walk.blelog
cargo run -p blelog-cli -- export jsonl walk.blelog -o walk.jsonl
```

### Two capture profiles

`--profile discovery` records raw BLE advertisements/scan responses and Wi-Fi
management frames (including beacons, probes and action frames). `--profile raw`
also records driver-delivered Wi-Fi data, control and metadata-only miscellaneous
reports. Raw is the default; neither profile deduplicates. Encrypted bytes remain
encrypted. Failed-FCS delivery is experimental and requires `--failed-fcs`.

```sh
cargo build -p blelog-cli
./target/debug/blelog-cli capture stop --port /dev/ttyACM0
./target/debug/blelog-cli capture start --port /dev/ttyACM0 --profile discovery --country FI
./target/debug/blelog-cli watch --port /dev/ttyACM0 --interval 1
```

Ctrl-C exits watch. Only one serial client may own the port at a time. Change
`discovery` to `raw` for maximum packet content; stop before changing settings.
Use the country where the board operates. The default `01` is world-safe 1–11;
the driver validates the selected country and channels. `--channels 6` locks
Wi-Fi to channel 6. `--radios ble|wifi|both` selects the enabled radios.

The shared radio alternates 1 s BLE and five 200 ms Wi-Fi channel dwells by
default. The channel cursor continues between bursts, with ±20 ms dwell jitter.
`--timing ble-priority` uses 1 s BLE / two Wi-Fi dwells; `wifi-priority` uses
500 ms BLE / five dwells. Override with `--ble-ms`, `--wifi-dwell-ms` and
`--channels-per-burst`. BLE uses active 100/100 ms scanning inside its window;
`--mode passive` disables BLE scan requests. No Wi-Fi association/probing occurs.

### Standalone powerbank use

Save settings explicitly to use them after unplugging/reboot:

```sh
./target/debug/blelog-cli config save --port /dev/ttyACM0 --profile raw --country FI
./target/debug/blelog-cli config show --port /dev/ttyACM0
```

`config save` stops recording and preserves the log; the next clean boot
autostarts the saved profile. Ordinary `capture start` settings are session-only.
First boot defaults to raw/both/balanced. `prepare --profile discovery --save`
performs stop, confirmed erase, clock sync, profile save and capture start.
`prepare` is destructive; `pull` never erases. Wall time is lost on a power cycle,
so an unsynchronized outdoor session has monotonic timestamps and null Unix time.

The 12.94 MiB log fills in approximately 22.6 minutes at 10 kB/s, 4.5 minutes at
50 kB/s or 2.3 minutes at 100 kB/s. These are capacity estimates, not measured
rates. Watch reports actual byte rate, remaining-time estimate, radio/channel,
BLE/Wi-Fi queue drops, time in listening windows and switching time. The counters
cannot count RF packets the drivers never delivered. FULL stops recording and
never overwrites earlier data. Interrupted writes can leave CORRUPT_TAIL; pull
the valid prefix before explicitly erasing to enable capture again.

The internal-RAM queues hold 128 BLE reports and 8 Wi-Fi frames of up to 4095
bytes. These bounded capacities leave memory for both drivers. Overflow drops a
whole report/frame and increments its counter; it never shortens a stored payload.

### Download and export

```sh
./target/debug/blelog-cli pull --port /dev/ttyACM0 --output walk.blelog
./target/debug/blelog-cli verify walk.blelog
./target/debug/blelog-cli export jsonl walk.blelog -o walk.jsonl
./target/debug/blelog-cli export csv walk.blelog --radio ble -o walk-ble.csv
./target/debug/blelog-cli export csv walk.blelog --radio wifi -o walk-wifi.csv
./target/debug/blelog-cli export pcapng walk.blelog --radio wifi -o walk-wifi.pcapng
```

Pull stops both radios and leaves capture stopped; use `--resume` for an
interrupted `.part` download. Preserve the canonical `.blelog`. JSONL has `radio`
fields; mixed CSV requires a radio selection. PCAPNG exports Wi-Fi byte frames,
not BLE HCI reports or Wi-Fi metadata-only reports. Unsynchronized PCAPNG packets
use a synthetic epoch from monotonic microseconds, explicitly noted in packet
comments; displayed calendar dates then have no meaning. Incomplete Wi-Fi frames
remain in the canonical archive but are counted and excluded from packet export.
Export refuses to replace existing files.

Hardware qualification is still in progress. A controlled test exposed a storage
stack overflow; large Wi-Fi queue receives now use an aligned internal buffer.
The revised build passed 20 alternating-profile starts and 40 stop calls. Dense
traffic/long-soak qualification is still required. The initial board sample has
driver-delivered trailer bytes that fail software FCS verification; PCAPNG marks
that mismatch. Preserve the canonical bytes and do not interpret this alone as
RF corruption. See [hardware validation](HARDWARE-VALIDATION.md) before field use.

For a one-cable UART bench test, `cargo +esp firmware-build --features
radio-selftest` builds an explicit diagnostic variant: 20 alternating-profile
sessions / 40 stop calls, then stopped. It appends records but never erases or
changes boot settings, and requires a clean appendable log. Rebuild without the
feature and reflash before field use.

The S3 receives 2.4 GHz Wi-Fi and BLE advertisements, not 5/6 GHz, Bluetooth
Classic, or arbitrary established BLE connections. HT20 capture and the selected
channel/radio schedule leave blind periods. Raw means the bytes made available
by the driver, not a promise to record all RF traffic.

## Firmware

Install the Espressif Rust toolchain plus the pinned linker proxy once:

```sh
cargo install ldproxy --version 0.3.5 --locked
cargo +esp firmware-build
cargo +esp firmware-run
```

For an already provisioned logger, preserve NVS/boot settings as well as the log:

```sh
BLELOG_APP_ONLY=1 cargo +esp firmware-run
```

The default full provisioning image rewrites the boot/table/NVS region; it still
does not touch the log partition. Application-only updates require the existing
partition table and compatible bootloader from this repository.

The build pins ESP-IDF `v5.5.3`, targets `xtensa-esp32s3-espidf`, and flashes a
16 MiB QIO image with the partition table in `partitions.csv`. Native USB is the
binary protocol link; UART0 remains available for ESP-IDF logs. Set
`BLELOG_PORT` if the native USB device is not `/dev/ttyACM0`. To recover through
an onboard UART bridge, also set `BLELOG_BEFORE=default-reset`. The image helper
uses ESP-IDF's pinned partition generator and `esptool` so the custom data
subtype `0x40` and the bootloader's boot-safe flash header are retained.

To produce the merged image without flashing it:

```sh
scripts/make-firmware-image.sh
```

Before field use, verify the actual board:

```sh
espflash board-info --port /dev/ttyACM0 --before usb-reset
```

The firmware warns when the detected flash/PSRAM or `blelog` partition differs
from the expected ESP32-S3-WROOM-1-N16R8 configuration. Hardware soak and
power-loss qualification require a connected device and are not host tests.
