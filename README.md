# BLE Logger

ESP32-S3 BLE observation logger and host-side extraction tools. The logger keeps
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

`prepare` performs stop, explicitly confirmed erase, clock synchronization, and
an active 100 ms/100 ms capture start. `pull` never erases device data.

## Firmware

Install the Espressif Rust toolchain plus the pinned linker proxy once:

```sh
cargo install ldproxy --version 0.3.5 --locked
cargo +esp firmware-build
cargo +esp firmware-run
```

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
