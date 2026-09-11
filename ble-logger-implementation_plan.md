# ESP32-S3 BLE Observation Logger — Rust Implementation Plan

**Target:** ESP32-S3-WROOM-1-N16R8 development board  
**Primary goal:** Continuously scan Bluetooth Low Energy advertisements while walking, preserve each received raw advertisement payload plus useful metadata in persistent flash, then pull the captured data to a computer with a Rust CLI.  
**Language:** Rust  
**Phase:** 1 — BLE only. Wi-Fi scanning and CSI are intentionally out of scope.

---

## 1. Project objective

Build a portable BLE observation logger for the ESP32-S3 that:

1. Supports both passive and active BLE scanning, with **active scanning as the default sensor-capture mode** so scan-response data is collected when advertisers provide it.
2. Stores **every received observation as an individual record**; do not deduplicate by advertiser.
3. Preserves the **raw BLE advertising payload bytes and scan-response bytes** exactly as delivered by the BLE host stack.
4. Stores metadata useful for later analysis, especially timestamp, advertiser address, address type and RSSI.
5. Writes observations to a dedicated raw flash partition using an append-only binary format.
6. Survives resets and unexpected power loss without corrupting previously committed records.
7. Provides a wired serial protocol that a Rust desktop CLI can use to inspect, download and erase logs.
8. Exports downloaded records to at least JSONL and CSV on the host.
9. Reports dropped observations and flash/storage errors instead of silently losing data.

The first version should optimize for **correctness, raw-data preservation, recoverability and simplicity**, not sophisticated compression or filtering.

---

## 2. Hardware assumptions

Target module:

```text
ESP32-S3-WROOM-1-N16R8
```

Expected memory configuration:

```text
Flash: 16 MiB Quad SPI
PSRAM: 8 MiB Octal SPI
```

Espressif's current module documentation lists `ESP32-S3-WROOM-1-N16R8` as 16 MB flash + 8 MB PSRAM.

Reference:

- https://www.espressif.com/sites/default/files/documentation/esp32-s3-wroom-1_wroom-1u_datasheet_en.pdf
- https://developer.espressif.com/blog/2025/03/espressif-part-numbers-explained/

### Required runtime verification

Do not rely only on the product listing or module marking. At firmware startup:

- Query/log the detected flash size.
- Query/log whether PSRAM initialized successfully and its detected size.
- Verify the `blelog` partition exists and print its size.
- Emit a visible warning if the hardware does not match the expected 16 MiB flash / 8 MiB PSRAM configuration.
- Never calculate the BLE log partition by hard-coded absolute flash addresses inside the application. Locate it through the ESP-IDF partition API by label.

A useful boot message is:

```text
BLE Logger v0.1
chip              : ESP32-S3
flash detected    : 16 MiB
psram detected    : 8 MiB
blelog partition  : 12.94 MiB
record format     : v1
scan mode         : passive
append offset     : 0x00123456
valid records     : 18294
```

---

## 3. Recommended Rust architecture

### 3.1 Use Rust on ESP-IDF for phase 1

Use the Rust `std` + ESP-IDF stack rather than a pure `no_std`/`esp-hal` implementation for this first phase.

Recommended reasons:

- ESP32-S3 BLE is officially supported by ESP-IDF.
- ESP-NimBLE is a mature BLE host stack on ESP32-S3.
- The Rust `esp32-nimble` crate exposes BLE scanning and the received advertisement payload as raw bytes.
- ESP-IDF provides robust partition-bounded flash read/write/erase APIs.
- This avoids implementing HCI handling or a BLE host stack in phase 1.

Recommended starting template:

```bash
cargo generate esp-rs/esp-idf-template cargo
```

Select:

```text
MCU: esp32s3
ESP-IDF: a stable supported release, not master
Target: xtensa-esp32s3-espidf
```

Reference:

- https://github.com/esp-rs/esp-idf-template

### 3.2 BLE stack

Use:

```text
ESP Bluetooth Controller
        ↓
ESP-NimBLE
        ↓
esp32-nimble Rust wrapper
        ↓
scanner callback
```

At the time this plan was written, `esp32-nimble` 0.12.0 exposes:

- `BLEScan`
- `active_scan(...)`
- `filter_duplicates(...)`
- scan interval/window configuration
- `BLEAdvertisedData::payload() -> &[u8]`

References:

- https://docs.rs/crate/esp32-nimble/latest
- https://taks.github.io/esp32-nimble/esp32_nimble/struct.BLEScan.html
- https://taks.github.io/esp32-nimble/esp32_nimble/struct.BLEAdvertisedData.html
- https://docs.espressif.com/projects/esp-idf/en/latest/esp32s3/api-guides/ble/overview.html

### 3.3 Required NimBLE configuration

Start with:

```ini
CONFIG_BT_ENABLED=y
CONFIG_BT_BLE_ENABLED=y
CONFIG_BT_BLUEDROID_ENABLED=n
CONFIG_BT_NIMBLE_ENABLED=y
```

Extended advertising support should be enabled if `esp32-nimble` and the selected ESP-IDF combination work reliably with it:

```ini
CONFIG_BT_NIMBLE_EXT_ADV=y
```

ESP32-S3 supports extended BLE advertising. Do not make extended advertising a blocker for the first working milestone; legacy advertisement logging must work first.

---

## 4. Repository layout

Use a Cargo workspace so the firmware and extraction tool share the binary record definitions.

Suggested layout:

```text
ble-observation-logger/
├── Cargo.toml
├── README.md
├── partitions.csv
├── firmware/
│   ├── Cargo.toml
│   ├── build.rs
│   ├── sdkconfig.defaults
│   └── src/
│       ├── main.rs
│       ├── ble.rs
│       ├── storage.rs
│       ├── protocol.rs
│       ├── clock.rs
│       ├── stats.rs
│       └── error.rs
├── crates/
│   └── blelog-format/
│       ├── Cargo.toml
│       └── src/
│           ├── lib.rs
│           ├── record.rs
│           ├── transport.rs
│           └── crc.rs
└── tools/
    └── blelog-cli/
        ├── Cargo.toml
        └── src/
            ├── main.rs
            ├── serial.rs
            ├── download.rs
            ├── decode.rs
            └── export.rs
```

The shared `blelog-format` crate should contain only portable serialization/parsing logic and should have extensive host-side unit tests.

---

## 5. Flash partition design

### 5.1 Do not use NVS as the BLE observation log

NVS is appropriate for small configuration values, not a continuously appended stream of BLE observations.

Use a dedicated data partition named:

```text
blelog
```

Use NVS only for small configuration/state values if needed, for example:

- scan configuration
- optional clock metadata
- device settings

### 5.2 Proposed 16 MiB partition table

A good initial layout is:

```csv
# Name,     Type, SubType, Offset,   Size
nvs,        data, nvs,     0x9000,   0x6000
phy_init,   data, phy,     0xF000,   0x1000
factory,    app,  factory, 0x10000,  0x300000
blelog,     data, 0x40,    0x310000, 0xCF0000
```

This gives approximately:

```text
Firmware partition: 3.00 MiB
BLE log partition:  12.94 MiB
```

The agent must validate this table with the selected ESP-IDF/espflash toolchain before flashing.

There is no OTA requirement in phase 1. If OTA is later introduced, the log partition will need to shrink to make room for two application slots.

### 5.3 Access flash through ESP-IDF partition APIs

Use `esp-idf-sys` bindings to:

```text
esp_partition_find_first
esp_partition_read
esp_partition_write
esp_partition_erase_range
```

Do not write directly to arbitrary SPI flash addresses from the application.

ESP-IDF requires a flash region to be erased before writing it. Erase operations must respect the partition erase-size alignment.

Reference:

- https://docs.espressif.com/projects/esp-idf/en/release-v5.4/esp32/api-reference/storage/partition.html

---

## 6. Storage strategy

### 6.1 Phase-1 model: append-only pre-erased log

Use a linear append-only log.

```text
blelog partition
┌───────────────────────────────────────────────┐
│ record 0                                      │
├───────────────────────────────────────────────┤
│ record 1                                      │
├───────────────────────────────────────────────┤
│ ...                                           │
├───────────────────────────────────────────────┤
│ last valid record                             │
├───────────────────────────────────────────────┤
│ 0xFF 0xFF 0xFF ... unused erased flash       │
└───────────────────────────────────────────────┘
```

Do **not** implement a filesystem or circular ring buffer in phase 1.

Reasons:

- simplest failure model
- easy recovery after power loss
- straightforward host decoding
- maximum useful space
- avoids filesystem metadata writes
- records can be copied verbatim to a host file

### 6.2 Erase behavior

The preferred workflow is:

```text
1. User connects device at home.
2. `blelog erase` erases the dedicated log partition.
3. Device verifies that the beginning/end of the partition are erased.
4. User disconnects USB and takes device outside.
5. Device appends records only; no erase is required while walking.
6. User returns and downloads the log.
```

Do not automatically erase old logs on boot.

Never erase the log partition while active scanning is running unless explicitly requested by the user and scanning has first been stopped.

### 6.3 Flash wear

This design naturally distributes writes across the entire ~13 MiB log region. Flash erase wear occurs primarily when the user explicitly clears the partition.

Avoid per-record NVS writes or metadata-sector updates because those would concentrate wear on a small number of sectors.

### 6.4 Full-partition behavior

When there is insufficient space for the next complete record:

- do not overwrite old data
- do not wrap around
- atomically transition logger state to `FULL`
- increment a `records_dropped_storage_full` counter for subsequent BLE observations
- continue running the serial control interface
- print/log a clear `BLE LOG FULL` warning

A circular buffer can be a later feature, but it is intentionally excluded from phase 1.

---

## 7. BLE observation requirements

### 7.1 Preserve every observation

Configure scanning with duplicate filtering disabled:

```rust
scan.filter_duplicates(false);
```

Do not maintain a map keyed by MAC address.

If the same advertiser is seen 500 times, phase 1 should be capable of storing 500 observations.

This is important because RSSI changes, payload changes and advertisement timing are useful analysis data.

### 7.2 Raw advertisement payload is mandatory

For every scan callback, copy:

```rust
data.payload()
```

into the observation record without modifying, normalizing or re-encoding it.

Parsed fields such as manufacturer data, service UUIDs or device name are optional conveniences for host-side analysis and should normally be derived later from the saved raw bytes.

**Source of truth = raw payload bytes.**

### 7.3 Do not parse in the BLE callback

The BLE callback should do as little work as possible:

```text
BLE callback
   ↓
read metadata
   ↓
copy raw bytes
   ↓
enqueue Observation
   ↓
return immediately
```

Do not:

- write flash directly in the callback
- serialize JSON
- print every packet
- parse every AD structure
- perform expensive allocation-heavy transformations

This prevents the BLE host callback from becoming the throughput bottleneck.

### 7.4 Scan modes and scan-response capture

Support two modes, but treat **active scanning as a first-class phase-1 requirement**.

#### Active scan — default sensor-capture mode

```text
active_scan(true)
filter_duplicates(false)
```

Active scanning sends a scan request after receiving an advertisement from a scannable advertiser. The advertiser may then return a **scan response containing additional raw AD structures** such as device names, service data, manufacturer data or other sensor/vendor-specific bytes.

Because the purpose of this logger is later analysis of unknown sensors and BLE devices, the default walking profile should use active scanning unless the user explicitly selects passive mode.

The implementation must preserve both components:

```text
Advertising report
        +
Scan-response report (when available)
```

**Do not silently discard scan-response data. Do not parse it and keep only selected fields. Preserve its raw bytes.**

Where the BLE/NimBLE API exposes advertisement and scan response as distinct events, store them as distinct `BleObservation` records and mark the record kind explicitly. Records from the same advertiser can later be correlated on the host by address, timestamps and sequence order.

Where the selected high-level Rust API returns a combined advertisement + scan-response buffer instead of two distinct events, the firmware must preserve enough metadata to reconstruct the boundary, for example `adv_payload_len` and `scan_response_len`. If the high-level wrapper cannot expose this distinction reliably, use a minimal lower-level ESP-IDF/NimBLE binding for the scan descriptor rather than accepting loss of scan-response provenance.

The agent must validate the behavior on the actual ESP32-S3 build with a known scannable test advertiser before considering active-scan support complete.

#### Passive scan — configurable alternative

```text
active_scan(false)
filter_duplicates(false)
```

Passive mode receives advertisements without sending scan requests. Keep it available for lower RF activity, lower power use, and experiments where the logger should remain receive-only. In passive mode, scan-response records are naturally absent.

Expose the scan mode through firmware configuration and the host CLI. The active/passive mode used for a capture session must be stored in `SessionStart`.

### 7.5 Scan interval/window

Initial high-capture profile:

```text
interval = 100 ms
window   = 100 ms
```

This gives effectively continuous scanning from the host configuration perspective.

Make both configurable. Later tests should determine whether a lower duty cycle is preferable for battery runtime.

### 7.6 Extended advertisements

The binary file format must support payload lengths larger than 31 bytes even if the first firmware milestone only proves legacy advertising.

Use a `u16` payload length.

If extended advertising is enabled, test:

- legacy ADV payload
- **legacy scan-response payload from a known test advertiser**
- verify that ADV and SCAN_RSP can be distinguished or their byte boundary is preserved
- extended advertisement payload
- maximum payload actually delivered by the selected NimBLE API

Do not assume that the wrapper reconstructs every chained over-the-air extended advertising fragment into one complete payload. Document the exact behavior observed in testing.

---

## 8. Record format v1

Use an explicitly versioned binary format.

### 8.1 Design requirements

Every record must be:

- independently identifiable
- length-delimited
- CRC protected
- skippable by a decoder that does not understand its record type
- recoverable after an interrupted final write
- little-endian

Do not serialize Rust structs by raw memory layout or `transmute`. Padding and representation are not a stable file format.

### 8.2 Record envelope

Recommended envelope:

```text
Offset  Size  Field
------  ----  ----------------------------------
0       4     magic = ASCII "BLER"
4       1     format_version = 1
5       1     record_type
6       2     header_len
8       4     total_record_len
12      ...   record-specific header
...     N     payload
...     4     CRC32 of all bytes except CRC field
```

`total_record_len` includes the envelope, record-specific header, payload and trailing CRC.

Maximum accepted record length must be bounded by the decoder to defend against corrupt length fields.

### 8.3 Record types

Version 1 should define at least:

```text
0x01 = SessionStart
0x02 = BleObservation
0x03 = SessionEnd
```

Unknown record types must be skipped using `total_record_len` after basic validation.

### 8.4 SessionStart record

Store once each time capture begins.

Suggested fields:

```text
session_id             u64
firmware_version       fixed/length-prefixed bytes
format_version         u8
boot_id                u32
start_monotonic_us     u64
wall_clock_unix_ms     i64   (-1 if unknown)
scan_mode              u8    (0 passive, 1 active)
scan_interval_ms       u16
scan_window_ms         u16
extended_adv_enabled   u8
```

A randomly generated `session_id` is useful for grouping records after multiple walks.

### 8.5 BleObservation record

Minimum required fields:

```text
sequence                u32
session_id              u64
monotonic_timestamp_us  u64
address                  [u8; 6]
address_type             u8
rssi_dbm                 i8
payload_len              u16
raw_payload              [u8; payload_len]
```

Reserve metadata fields where available from the BLE stack:

```text
report_kind              u8    (advertisement / scan response / combined / unknown)
event_flags / adv type   u16
adv_payload_len          u16    (0xFFFF if unavailable)
scan_response_len        u16    (0xFFFF if unavailable)
primary_phy              u8
secondary_phy            u8
advertising_sid          u8
tx_power_dbm             i8
periodic_interval        u16
```

Use explicit sentinel values for unavailable metadata, for example:

```text
PHY unknown         = 0xFF
SID unknown         = 0xFF
TX power unknown    = 0x7F
```

Do not fake metadata the wrapper does not actually expose.

If `esp32-nimble` does not expose a required metadata field, choose one of these in order:

1. leave the field unknown in v1
2. add a very small safe wrapper around the underlying ESP-IDF/NimBLE scan descriptor
3. avoid forking `esp32-nimble` unless absolutely necessary

Raw payload, scan-response preservation, advertiser address and RSSI are the phase-1 priorities.

### 8.6 SessionEnd record

When capture is stopped cleanly, append:

```text
session_id
stop_monotonic_us
total_observations
dropped_queue_full
dropped_storage_full
flash_write_errors
```

Absence of `SessionEnd` must be valid and simply mean the device lost power or reset before a clean stop.

### 8.7 CRC and crash recovery

Use CRC32, e.g. CRC-32/ISO-HDLC, with one implementation shared between firmware and host.

On boot/recovery:

1. Start at offset 0 of `blelog`.
2. If bytes are erased (`0xFF`) at the expected record boundary, that is the append point.
3. Validate magic.
4. Validate version and plausible length.
5. Ensure record stays inside partition boundaries.
6. Read the complete record.
7. Validate CRC.
8. Advance by `total_record_len`.
9. Stop at the first invalid/incomplete record.

An interrupted final flash write therefore loses only that final record. Previously valid records remain usable.

Do not attempt to repair or overwrite the incomplete tail automatically. For phase 1, mark the remaining log unusable for further append after detecting a partially programmed corrupt tail, and require download + erase. This avoids unsafe flash rewrites over non-erased bits.

---

## 9. Timestamp strategy

The ESP32-S3 board does not provide a battery-backed RTC by default, so separate monotonic and wall-clock time.

### Always store

```text
monotonic_timestamp_us
```

Obtain it from ESP-IDF's monotonic timer (`esp_timer_get_time()` or safe binding equivalent).

### Optional wall-clock base

The host CLI should support:

```bash
blelog-cli clock sync --port <PORT>
```

This sends the current Unix time to the device. The firmware stores:

```text
unix_ms_at_sync
monotonic_us_at_sync
```

While the device remains powered, absolute observation time can then be calculated as:

```text
unix_ms = unix_ms_at_sync
        + (observation_monotonic_us - monotonic_us_at_sync) / 1000
```

If no clock sync occurred, host export should leave wall time empty and retain monotonic time.

Never invent absolute time after a reboot for which no time source is available.

---

## 10. Firmware concurrency model

Use separate responsibilities/tasks.

```text
                ┌────────────────────┐
BLE radio ----> │ NimBLE scan callback│
                └─────────┬──────────┘
                          │ Observation
                          ▼
                  bounded queue
                          │
                          ▼
                ┌────────────────────┐
                │ Storage writer task │
                │ serialize + buffer  │
                │ append to flash     │
                └────────────────────┘

USB/UART <----> Control / dump task
```

### 10.1 Observation queue

Use a bounded queue/channel so memory consumption cannot grow without limit.

Start with a capacity equivalent to roughly 256–1024 normal observations and tune from measured traffic.

If the queue is full:

- never block the BLE callback for a long time
- drop the new observation
- increment `dropped_queue_full`

Expose the counter through `stats` and include it in exports/session metadata.

A drop counter is preferable to hidden packet loss.

### 10.2 RAM/PSRAM write buffer

Use a write-combining buffer between serialization and flash, for example:

```text
16 KiB to 64 KiB
```

The 8 MiB PSRAM gives ample headroom, but the first implementation does not need to allocate megabytes.

Requirements:

- flush when buffer is nearly full
- flush when capture stops
- flush before entering dump mode
- optionally flush on a low-frequency timer such as every 1 second so data is not held in RAM indefinitely

The agent should verify whether the chosen allocation lands in internal RAM or PSRAM. Correctness must not depend on PSRAM unless explicitly configured and tested.

### 10.3 Logging discipline

Do not print one line per advertisement in production capture mode. Serial logging can become a major bottleneck.

Recommended periodic status output only:

```text
obs=24581 stored=24492 queue_drop=89 flash_err=0 used=1.82MiB free=11.12MiB
```

Rate limit this to approximately once every 5–10 seconds when debugging; it may be disabled for normal use.

---

## 11. Serial extraction/control protocol

The user needs to be able to return home, connect the ESP32 over USB, and pull the observations out without putting the board into bootloader mode.

Implement an application-level binary serial protocol.

### 11.1 Transport

Recommended:

```text
UART/USB serial
baud: 921600 if stable
fallback: 460800 or 115200
```

Use whichever serial interface is exposed reliably by this specific DevKit clone. The host tool must list available serial ports.

### 11.2 Framing

Use COBS with `0x00` frame delimiter or another similarly robust framing method.

Recommended decoded frame:

```text
protocol_version    u8
message_type        u8
request_id          u16
payload_len         u32
payload             [u8; payload_len]
crc32               u32
```

Do not mix unrestricted debug text and binary frames on the same serial stream while the extraction protocol is active.

### 11.3 Required commands

Implement:

```text
PING
INFO
STATS
START_CAPTURE
STOP_CAPTURE
SET_CLOCK
DUMP_BEGIN
DUMP_READ
DUMP_END
ERASE_LOG
```

#### `INFO`

Return:

```text
firmware version
record format version
chip model
flash size
PSRAM size
log partition size
append offset
free bytes
valid record count
logger state
scan configuration
```

#### `STATS`

Return:

```text
received observations
stored observations
queue drops
storage-full drops
flash write errors
queue high-water mark
bytes written
```

#### `DUMP_BEGIN`

Requirements:

- stop scanning or reject dump while scanning
- flush pending write buffer
- freeze append offset
- return exact number of valid log bytes
- assign a dump/session transfer ID

#### `DUMP_READ`

Host sends:

```text
offset
requested_length
```

Device returns a chunk containing:

```text
offset
actual_length
bytes
chunk CRC
```

Use chunk sizes around 1024–4096 bytes and benchmark.

The protocol should allow the host to retry a failed chunk and resume from a known byte offset.

#### `ERASE_LOG`

Must require an explicit command and only execute while capture is stopped.

After erase:

```text
append offset = 0
record count = 0
state = EMPTY
```

### 11.4 Why not only use `espflash read-flash`?

`espflash` supports reading SPI flash and is useful as a recovery/development tool, but requiring ROM bootloader mode is inconvenient for normal data extraction.

The dedicated application protocol should be the normal workflow.

Keep `espflash read-flash` documented as a recovery/debugging fallback if the firmware cannot boot.

Reference:

- https://docs.rs/crate/espflash/latest/source/README.md

---

## 12. Host-side Rust tool

Binary name:

```text
blelog-cli
```

Use common crates such as:

```text
clap          CLI arguments
serialport    serial access
crc32fast     CRC if compatible with selected format
serde         host export types
serde_json    JSONL output
csv           CSV output
indicatif      optional progress bar
anyhow        CLI-level error handling
thiserror     structured library errors
```

Pin versions in `Cargo.lock` after the initial implementation is working.

### 12.1 Commands

Target command set:

```bash
blelog-cli ports
blelog-cli info --port /dev/ttyACM0
blelog-cli stats --port /dev/ttyACM0
blelog-cli clock sync --port /dev/ttyACM0
blelog-cli capture start --port /dev/ttyACM0
blelog-cli capture stop --port /dev/ttyACM0
blelog-cli pull --port /dev/ttyACM0 --output walk-001.blelog
blelog-cli verify walk-001.blelog
blelog-cli export jsonl walk-001.blelog -o walk-001.jsonl
blelog-cli export csv walk-001.blelog -o walk-001.csv
blelog-cli erase --port /dev/ttyACM0
```

On macOS, ports may appear similar to:

```text
/dev/cu.usbmodem*
/dev/cu.usbserial-*
```

Linux may use:

```text
/dev/ttyACM0
/dev/ttyUSB0
```

### 12.2 Pull behavior

`pull` should:

1. Open serial port.
2. Send `INFO`.
3. Refuse if protocol/record format is unsupported.
4. Send `DUMP_BEGIN`.
5. Create `<name>.part` locally.
6. Request chunks sequentially.
7. Validate every transport chunk CRC.
8. Persist progress.
9. Retry transient failures.
10. Finish with `DUMP_END`.
11. Validate the complete `.blelog` record stream.
12. Rename `.part` to `.blelog` only after validation.
13. Print record count, bytes, corrupted-record count and session summary.

Do **not** automatically erase device data after a successful pull. Data deletion must be a separate explicit command.

### 12.3 Resume support

If a pull is interrupted, support:

```bash
blelog-cli pull --resume ...
```

Read the size of the `.part` file, validate its last complete record/chunk boundary, and continue from a safe offset.

This is especially useful when downloading many megabytes over serial.

---

## 13. Host export formats

### 13.1 Raw `.blelog`

The raw binary dump is the canonical archive.

Never discard it just because CSV/JSONL was generated.

### 13.2 JSONL

Recommended row example:

```json
{"session_id":"8f1c...","sequence":1843,"monotonic_us":782388412,"unix_ms":1789150043123,"address":"AA:BB:CC:DD:EE:FF","address_type":"random","rssi_dbm":-67,"report_kind":"scan_response","raw_payload_hex":"0b094d7953656e736f72..."}
```

Include additional PHY/event metadata where available.

### 13.3 CSV

Recommended columns:

```text
session_id
sequence
monotonic_timestamp_us
unix_timestamp_ms
address
address_type
rssi_dbm
report_kind
event_flags
adv_payload_len
scan_response_len
primary_phy
secondary_phy
advertising_sid
tx_power_dbm
payload_len
raw_payload_hex
```

Use hexadecimal text for raw payload bytes.

### 13.4 Optional decoded AD fields

The host exporter may later add derived columns such as:

```text
local_name
manufacturer_company_id
manufacturer_data_hex
service_uuids
service_data
flags
```

These must be derived from `raw_payload` and must never replace it.

---

## 14. Expected capacity

The exact record size is variable because the raw payload is variable.

Assume roughly 40 bytes of record/envelope metadata plus payload and CRC.

### Example with a 31-byte legacy payload

Approximate record:

```text
40 B metadata/envelope
31 B raw payload
 4 B CRC
-----------------
~75 B total
```

With a ~12.94 MiB log partition:

```text
~13,565,952 bytes / 75 bytes
≈ 180,000 observations
```

The actual count will vary with:

- extended advertisement payload length
- record header size chosen by implementation
- alignment/padding policy
- session records

The firmware's `INFO`/`STATS` response should therefore report byte usage, not only estimated record capacity.

### Rough time examples

At 20 stored observations/sec:

```text
180,000 / 20 ≈ 2.5 hours
```

At 50/sec:

```text
180,000 / 50 ≈ 1 hour
```

At 100/sec:

```text
180,000 / 100 ≈ 30 minutes
```

These are illustrative only. Real BLE density varies dramatically by location and scan settings.

---

## 15. Boot and state machine

Define explicit logger states:

```text
BOOTING
RECOVERING
READY
CAPTURING
STOPPING
DUMPING
FULL
CORRUPT_TAIL
ERASING
ERROR
```

Suggested startup flow:

```text
boot
 ↓
initialize ESP-IDF + logging
 ↓
verify flash/PSRAM
 ↓
find blelog partition
 ↓
scan records / recover append offset
 ↓
initialize NimBLE
 ↓
READY
```

Do not start BLE scanning until storage recovery has completed.

### Autostart

For a useful portable logger, support a compile-time/config option:

```text
autostart_capture = true
```

When enabled:

- after successful boot + recovery, append a new `SessionStart`
- begin scanning automatically

This means the user can power the device from a battery and simply begin walking.

Default during development may be `false`; default for field-use build can become `true` after stability testing.

---

## 16. Error handling

No panic should be caused by malformed flash records or malformed serial commands.

Define structured errors for:

```text
BLE initialization
BLE scan start/stop
queue full
partition not found
partition size mismatch
flash read
flash write
flash erase
record encode
record CRC
record bounds
serial framing
protocol version mismatch
invalid command/state
```

Fatal hardware/storage initialization errors may stop capture, but the control interface should remain available whenever practical so the host can retrieve diagnostics.

---

## 17. Tests

### 17.1 Host unit tests — mandatory

The shared format crate must have tests for:

- serialize/deserialize `SessionStart`
- serialize/deserialize `BleObservation`
- zero-length payload
- 31-byte payload
- >31-byte payload
- maximum configured payload length
- CRC mismatch
- invalid magic
- unsupported version
- truncated header
- truncated payload
- truncated CRC
- malicious huge `total_record_len`
- unknown record type skipping
- concatenated records
- trailing erased `0xFF`
- interrupted final record

Use golden binary test vectors committed to the repository.

### 17.2 Storage recovery tests

Create a host-side simulation of a flash byte array initialized to `0xFF`.

Test power loss after every meaningful write boundary:

```text
record header partly written
header complete, payload partial
payload complete, CRC missing
CRC partial
full valid record
```

Recovery must never treat an incomplete record as valid.

### 17.3 Device integration tests

Test on the physical ESP32-S3:

1. Scan known BLE advertiser from a phone/beacon.
2. Confirm repeated advertisements are recorded when duplicate filtering is disabled.
3. Confirm RSSI varies across observations.
4. Confirm raw payload from firmware equals raw payload exposed by NimBLE callback.
5. Power-cycle during capture.
6. Verify earlier records survive.
7. Fill enough data to cross many 4 KiB flash sectors.
8. Download and verify entire log.
9. Erase and verify partition appears empty.
10. Run capture for at least 1–2 hours in a BLE-dense environment.
11. Observe queue high-water mark and drop counters.

### 17.4 Throughput test

Create a synthetic writer benchmark independent of BLE that pushes records at controlled rates:

```text
25 records/sec
50 records/sec
100 records/sec
200 records/sec
```

Measure:

```text
records/sec sustained
bytes/sec flash throughput
queue high-water mark
drops
average flush duration
maximum flush duration
```

This separates flash pipeline performance from BLE reception behavior.

---

## 18. Implementation milestones

### Milestone 0 — project/toolchain bootstrap

Deliverables:

- Rust ESP-IDF project builds for `xtensa-esp32s3-espidf`
- firmware flashes and boots
- custom 16 MiB partition table works
- firmware prints detected flash/PSRAM
- firmware finds `blelog` by partition label

Acceptance:

```text
cargo build succeeds
firmware boots repeatedly
blelog partition size is correct
```

### Milestone 1 — BLE scan proof

Deliverables:

- NimBLE initializes
- passive scan works
- duplicate filtering disabled
- callback receives address, RSSI and raw payload
- periodic counter logging only

Acceptance:

```text
same advertiser appears repeatedly
raw payload is visible in a temporary debug sample
no per-packet logging in final path
```

### Milestone 2 — shared binary format

Deliverables:

- `blelog-format` crate
- v1 envelope
- session + observation records
- CRC implementation
- complete host unit tests

Acceptance:

```text
all parser corruption/truncation tests pass
```

### Milestone 3 — append-only flash logger

Deliverables:

- partition erase command/helper
- append writer
- queue between BLE callback and writer
- write-combining buffer
- boot-time tail recovery
- storage full handling

Acceptance:

```text
BLE observations survive reset/power loss
previous valid records remain readable after interrupted final write
```

### Milestone 4 — serial control protocol

Deliverables:

- framed binary protocol
- `PING`, `INFO`, `STATS`
- start/stop capture
- dump begin/read/end
- erase
- clock sync

Acceptance:

```text
host can retrieve arbitrary ranges reliably
bad CRC/frame does not crash firmware
```

### Milestone 5 — Rust extraction CLI

Deliverables:

- port listing
- device info
- stats
- clock sync
- pull
- pull resume
- verify
- CSV export
- JSONL export
- erase

Acceptance:

```text
one command downloads a complete walk log
raw file verifies successfully
CSV/JSONL record counts match raw file
```

### Milestone 6 — field test

Procedure:

1. Erase logger.
2. Sync clock.
3. Disconnect from computer.
4. Power device from battery.
5. Walk for 30–60 minutes.
6. Return home.
7. Connect USB.
8. Stop capture if still active.
9. Pull `.blelog`.
10. Verify.
11. Export JSONL/CSV.
12. Inspect drop counters.

Success criteria:

```text
no flash corruption
no unexplained reboot
raw advertisement payloads present
scan-response payloads present when using a known scannable test advertiser
advertisement/scan-response provenance is verifiable in the export
valid timestamps
reasonable RSSI values
record count > 0
drop count = 0 ideally, otherwise quantified
```

---

## 19. Phase-1 acceptance criteria

The implementation is considered complete when all of these are true:

- [ ] Compiles and runs on ESP32-S3-WROOM-1-N16R8.
- [ ] Runtime reports 16 MiB flash and usable PSRAM on the target board.
- [ ] Dedicated `blelog` partition is created and located by label.
- [ ] BLE scanning operates without connecting to advertisers.
- [ ] Duplicate filtering is disabled.
- [ ] Active scanning is implemented and is the default sensor-capture profile.
- [ ] Passive scanning remains selectable.
- [ ] Scan-response data is captured when a scannable advertiser returns it.
- [ ] Advertisement vs scan-response provenance is retained (`report_kind`, or explicit ADV/SCAN_RSP lengths when the stack combines them).
- [ ] Each observation stores its raw advertising/scan-response payload unchanged.
- [ ] Advertiser address, address type and RSSI are stored.
- [ ] Every observation gets a monotonic timestamp.
- [ ] Optional host clock sync supports absolute timestamps during a powered session.
- [ ] BLE callback never performs direct flash writes.
- [ ] Bounded queue exists between scanner and storage writer.
- [ ] Queue drops are counted.
- [ ] Flash writes are sequential and append-only.
- [ ] Records contain a stable version, length and CRC.
- [ ] Firmware recovers the valid log tail after reset/power loss.
- [ ] Storage-full state never overwrites old records.
- [ ] Rust CLI can show device info and statistics.
- [ ] Rust CLI can pull a log without entering ROM bootloader mode.
- [ ] Interrupted pulls can be resumed or safely restarted.
- [ ] CLI verifies record CRCs.
- [ ] CLI exports JSONL.
- [ ] CLI exports CSV.
- [ ] Erasing logs is explicit and separate from downloading.
- [ ] A 30–60 minute outdoor/field capture has been completed successfully.

---

## 20. Explicit non-goals for phase 1

Do **not** spend time implementing these until the BLE logger is stable:

- Wi-Fi AP scanning
- Wi-Fi CSI
- Wi-Fi/BLE coexistence scheduling
- GPS
- web UI
- phone app
- cloud upload
- automatic home-Wi-Fi upload
- live BLE dashboard
- SQL database on device
- LittleFS/FATFS storage
- circular overwrite/ring buffer
- advertisement deduplication
- payload compression
- device fingerprinting/classification
- manufacturer lookup database on device
- GATT connections to discovered devices
- OTA updates
- encryption of captured logs
- PCAP/PCAPNG reconstruction

Keep phase 1 narrow: **scan → preserve raw advertisement → append safely → pull to host → decode/export.**

---

## 21. Privacy and data handling

BLE advertisements are broadcasts, but captured records can contain persistent or rotating identifiers and application-specific payloads.

Implementation requirements:

- Keep capture local to the device by default.
- Do not automatically upload observations anywhere.
- Do not silently enrich captured addresses with external identity databases.
- Make erase explicit and reliable.
- Treat exported `.blelog`, JSONL and CSV files as potentially privacy-sensitive datasets.

This is primarily a product/data-handling constraint, not a change to the BLE scanning implementation.

---

## 22. Recommended first coding sequence for the programming agent

Implement in this order:

```text
1. Generate ESP-IDF Rust project targeting ESP32-S3.
2. Add custom 16 MiB partition table.
3. Print/verify flash + PSRAM + blelog partition at boot.
4. Add esp32-nimble and prove passive scanning.
5. Disable duplicate filtering.
6. Copy raw `data.payload()` plus address/RSSI into an Observation object.
7. Build and thoroughly test the shared binary record crate on the host.
8. Add a bounded observation queue.
9. Implement raw append-only flash writer using esp_partition_* APIs.
10. Implement boot-time record recovery.
11. Implement explicit erase.
12. Implement serial framing and INFO/STATS.
13. Implement DUMP_BEGIN/DUMP_READ/DUMP_END.
14. Implement the Rust host CLI `pull` command.
15. Implement raw-file verifier.
16. Implement JSONL + CSV export.
17. Add clock sync.
18. Perform power-loss tests.
19. Perform high-density BLE/throughput tests.
20. Perform a real walking field test.
```

Do not begin Wi-Fi or CSI work before the phase-1 acceptance criteria pass.

---

## 23. Notes for the programming agent

### Important: raw payload semantics

`BLEAdvertisedData::payload()` is the raw advertisement data exposed by NimBLE to the host callback. It is **not necessarily the complete over-the-air Bluetooth Link Layer PDU**, including every PHY/link-layer header byte.

The requirement for this project is to preserve the raw advertising data delivered to the BLE host/API exactly. Do not claim that the logger captures raw RF or full link-layer packets unless a later lower-level HCI/controller implementation proves that.

### Important: wrapper/API stability

Before implementing around private struct layouts, inspect the exact pinned `esp32-nimble` version. Prefer its public API.

If a metadata field is unavailable publicly:

```text
raw payload > address > RSSI > timestamp
```

are higher priority than adding unsafe access for optional metadata.

### Important: never silently deduplicate

The user explicitly wants raw BLE observation history. Duplicate controller filtering must remain disabled in capture mode.

### Important: preserve forward compatibility

The host decoder must use record version + type + length and must not assume every future record is a `BleObservation`. This will allow later versions to add Wi-Fi/CSI record types without replacing the BLE log container format.

---

## 24. References

Hardware and BLE:

- ESP32-S3-WROOM-1/WROOM-1U datasheet: https://www.espressif.com/sites/default/files/documentation/esp32-s3-wroom-1_wroom-1u_datasheet_en.pdf
- ESP32-S3 BLE overview: https://docs.espressif.com/projects/esp-idf/en/latest/esp32s3/api-guides/ble/overview.html
- ESP32-S3 RF coexistence: https://docs.espressif.com/projects/esp-idf/en/stable/esp32s3/api-guides/coexist.html

Rust:

- Rust ESP-IDF template: https://github.com/esp-rs/esp-idf-template
- `esp32-nimble`: https://github.com/taks/esp32-nimble
- `BLEScan`: https://taks.github.io/esp32-nimble/esp32_nimble/struct.BLEScan.html
- `BLEAdvertisedData`: https://taks.github.io/esp32-nimble/esp32_nimble/struct.BLEAdvertisedData.html
- `espflash`: https://github.com/esp-rs/espflash

Storage:

- ESP-IDF partition API: https://docs.espressif.com/projects/esp-idf/en/release-v5.4/esp32/api-reference/storage/partition.html

---

## Final architecture

```text
                      ESP32-S3-WROOM-1-N16R8

        ┌──────────────────────────────────────────┐
        │                  BLE RF                  │
        └────────────────────┬─────────────────────┘
                             │
                             ▼
                    ESP-NimBLE scanner
                             │
              raw payload + metadata
                             │
                             ▼
                     bounded queue
                 drop counter on overflow
                             │
                             ▼
                   storage writer task
                 serialize + CRC + batch
                             │
                             ▼
             ~12.94 MiB `blelog` partition
                    append-only flash
                             │
                             │ USB/UART
                             ▼
                    ┌────────────────┐
                    │  blelog-cli    │
                    └───────┬────────┘
                            │
             ┌──────────────┼──────────────┐
             ▼              ▼              ▼
        walk.blelog     walk.jsonl      walk.csv
       canonical raw      analysis       analysis
          archive
```

The architecture should remain this simple until it survives long real-world BLE capture sessions with zero unexplained corruption.
