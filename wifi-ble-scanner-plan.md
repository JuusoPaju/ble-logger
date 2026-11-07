# Wi-Fi + BLE logger expansion plan

Planning baseline: 2026-09-12. The firmware and CLI now implement the two-profile
design; device qualification is in progress. This document preserves the design
baseline. See README.md and FORMAT.md for implemented commands and wire layouts.
Existing BLE captures remain readable.

Implementation refinements: the existing v1 session records are retained with
additive configuration/window/statistics/Wi-Fi fragment records; the channel order
is cyclic with a randomized starting channel; failed-FCS delivery is opt-in pending
qualification. Hardware RAM limits require 128 BLE queue entries and 8 full-size
Wi-Fi entries in internal RAM, divided into small allocations. A temporary 32 KiB
internal recovery buffer is freed before radio initialization.

## Objective and hardware limits

Extend this workspace into a portable, standalone recorder of BLE advertising
reports and driver-delivered 2.4 GHz Wi-Fi frames, with one timeline, explicit
listening coverage, and the existing USB download workflow.

The previously tested board has an ESP32-S3, 16 MiB flash and 8 MiB PSRAM. The
Amazon product page could not be retrieved directly, so use those measured
properties and the chip documentation rather than unverified listing details.

The ESP32-S3 supports 2.4 GHz 802.11b/g/n and Bluetooth LE. It cannot provide
5/6 GHz Wi-Fi, Bluetooth Classic, Zigbee/Thread, arbitrary RF recordings or raw
IQ samples. BLE scanning observes advertising reports and, in active mode,
solicited scan responses; it is not a general sniffer for existing BLE
connections. Extended advertising support does not guarantee complete extended
advertising chains. See the [ESP32-S3 datasheet](https://documentation.espressif.com/esp32-s3_datasheet_en.pdf).

Wi-Fi and BLE share one radio, not just an antenna. Espressif's IDF 5.5.3 table
marks ordinary Wi-Fi scanning plus BLE scanning as stable, but Wi-Fi sniffer
reception plus BLE scanning as supported with unstable performance. Therefore
use explicit alternating capture windows initially; compare automatic
coexistence experimentally. A second antenna would not create a second radio.
See [RF coexistence](https://docs.espressif.com/projects/esp-idf/en/v5.5.3/esp32s3/api-guides/coexist.html).

“Everything” means everything delivered by the enabled drivers within the
selected radio/channel windows and storage limits. Packets outside those windows,
collisions, weak signals and unsupported PHY traffic remain unobservable.

## Capture profiles

Timing and frame selection are separate settings.

- `discovery`: optional lower-volume profile. Preserve BLE reports and raw Wi-Fi
  management frames, including beacons, probe requests/responses and action
  frames. Do not deduplicate or parse SSIDs in the callback. Host export derives
  AP/device summaries. This profile deliberately excludes Wi-Fi data/control
  frames and cannot discover every associated or quiet client.
- `raw`: user-selected default. Include management, data, control and
  metadata-only miscellaneous reports delivered by the Wi-Fi driver. Configure both the top-level filter
  and control-subtype filter. Include failed-FCS reports after validating their
  delivery and safe buffer handling on the pinned driver; expose their status
  explicitly and retain a switch to exclude them.
  Preserve encrypted bytes as received; do not promise plaintext application
  traffic. This is the maximum-content, shorter-duration profile. No deduplication,
  sampling, summary substitution or intentionally shortened snap length by default.
- `ble-only` and `wifi-only`: retain dedicated modes for use and benchmarking.
  Wi-Fi-only also supports locking to one channel.

Wi-Fi uses promiscuous reception in `WIFI_MODE_NULL`, without association or
active AP discovery. A normal AP scan returns a network inventory, not this raw
packet stream. ESP-IDF exposes management/data/control frames and some
metadata-only reports; not all received PHY traffic has accessible bytes.
See [Wi-Fi sniffer mode](https://docs.espressif.com/projects/esp-idf/en/v5.5.3/esp32s3/api-guides/wifi.html#wi-fi-sniffer-mode).

BLE remains active by default, as in the existing logger, to obtain scan
responses. Active BLE sends scan requests. An explicit passive option makes both
capture paths listen-only, at the cost of solicited BLE scan-response data.

## Initial radio schedule

Start with this repeated schedule:

```text
BLE 1,000 ms | Wi-Fi ch A 200 ms | B 200 | C 200 | D 200 | E 200 | repeat
```

Advance through the entire configured channel list across Wi-Fi windows; do not
restart at channel 1 each cycle. Use each channel equally. A shuffled cyclic
permutation chosen once per session avoids privileging channels at startup while
keeping revisit gaps bounded. Add small bounded Wi-Fi dwell jitter, initially
±20 ms, to reduce repeated alignment with periodic transmitters. Save the seed
and actual window times for reproducibility.

The following are candidate timings, not measured optima. Shares omit handoff
time; revisit values assume 13 channels with equal dwell and no jitter.

| Timing profile | BLE window | Wi-Fi window | BLE / Wi-Fi share | Mean per-channel revisit |
|---|---:|---:|---:|---:|
| Balanced, initial default | 1,000 ms | 5 × 200 ms | 50% / 50% | 5.2 s |
| BLE priority | 1,000 ms | 2 × 200 ms | 71% / 29% | 9.1 s |
| Wi-Fi priority | 500 ms | 5 × 200 ms | 33% / 67% | 3.9 s |

Mean revisit = channel count × cycle duration / channels visited per cycle.
Balanced scheduling gives each of 13 channels approximately 3.85% of total
listening time, not 50%. Its individual channel start-to-start revisits are
roughly 4.6 or 5.6 seconds without overhead, with a 5.2-second average.

A 200 ms dwell is a reasonable starting point for periodic beacons: a 100-TU
beacon interval is 102.4 ms. It offers more opportunity than a very short hop,
but does not guarantee reception, especially with longer beacon intervals or
contention. Test 120/200/300 ms dwells. Short, isolated probe requests can still
be missed completely. The 100-TU example follows the units/default in the
[IDF Wi-Fi types](https://github.com/espressif/esp-idf/blob/v5.5.3/components/esp_wifi/include/esp_wifi_types_generic.h).

Within BLE windows retain 100 ms interval/window and duplicate filtering off.
This requests continuous scanning during the window; it does not imply listening
on all BLE channels or PHYs simultaneously. The pinned NimBLE wrapper supplies
scan parameters for both uncoded and coded scanning when extended advertising
is enabled; qualify those paths separately on hardware.

Use IDF's country-code API and read back the permitted channel range. Until a
country is configured, keep the driver's world-safe channels 1–11; after an
appropriate country selection, include 12/13 when the driver permits them.
Do not restrict normal coverage to 1/6/11. Start with HT20; HT40 reception and
secondary-channel scheduling are a separately measured extension, not assumed
full coverage. See the [pinned Wi-Fi API](https://github.com/espressif/esp-idf/blob/v5.5.3/components/esp_wifi/include/esp_wifi.h).

## Repository changes

### Radio ownership and lifecycle

Add `firmware/src/radio.rs` and `firmware/src/wifi.rs`.

- `radio.rs` owns one capture session and alternates BLE/Wi-Fi windows. The
  controller owns this scheduler instead of a BLE-only thread. Window changes
  do not create new sessions or reset sequence counters.
- Refactor `ble::run_scan` into one bounded window plus a reusable callback.
  Preserve the existing workaround: external GAP cancellation does not wake the
  wrapper's pending scan in this configuration. Finish bounded scans normally.
  Use supported durations in 10 ms increments and keep STOP latency bounded.
- Initialize Wi-Fi once. For each Wi-Fi burst: start driver, set channel, enable
  promiscuous reception, run the requested dwells, disable reception, synchronize
  with callbacks, stop driver, then begin BLE. Reapply channel after restart.
  Do not repeatedly deinitialize either stack. Benchmark the start/stop cost;
  retaining Wi-Fi started but inactive is a later optimization only after
  proving that it does not reduce BLE reception.
- Channel switches gate new callback admission and wait for callbacks already
  admitted. Record driver-reported channel and a window ID on every observation;
  do not label a late callback using only the scheduler's current channel.
- On STOP/FULL/error, stop both producers, wait for their callbacks, drain both
  queues, write final statistics/end, then acknowledge. Close only one session.
  FULL and flash failures must reach the scheduler instead of calling only
  `ble::request_stop`, as the current storage path does.
- Preserve public logger states. Add separate radio substates (BLE, Wi-Fi,
  switching, idle), rather than changing the meaning of `CAPTURING` every second.
- Enable `CONFIG_ESP_COEX_SW_COEXIST_ENABLE` for the pinned IDF configuration.
  Inspect generated settings and place Wi-Fi and BLE protocol tasks on separate
  cores. Software coexistence remains enabled during handoffs; the application
  schedule is not a promise about exact hardware arbitration.
- Keep USB protocol service responsive during startup, switching and failures.
  Audit initialization so a Wi-Fi startup failure still leaves USB diagnostics.

### Callback, queues and memory

The existing BLE queue has 256 inline observations, each carrying up to 255
payload bytes. Do not expand every slot to a maximum-sized Wi-Fi packet.

Keep separate bounded BLE and Wi-Fi admission queues, with fair draining by the
single storage task. Use a preallocated Wi-Fi block pool and descriptors rather
than heap allocation per callback. Start memory budgeting at a 64 KiB Wi-Fi
payload pool (256 × 256-byte blocks), with a bounded descriptor queue; validate
the final allocation against Wi-Fi/NimBLE stacks and the existing BLE queue.
Reserve a small independent control-event queue for window and final records.

Explicitly allocate capture pools/queue storage in internal RAM and verify their
placement. The current generic allocator plus PSRAM-enabled configuration does
not by itself prove that the large crossbeam queue is internal. PSRAM can be an
optional staging optimization, never the only place holding a record promised
durable. If required internal allocations fail, report ERROR through USB.

Wi-Fi callback work is limited to timestamp/sequence assignment, public metadata
extraction, bounded copy, nonblocking enqueue and counters. Use the generated
ESP-IDF types/accessors or a small C adapter, not copied private bitfield layouts.
Keep all parsing, CRCs and flash operations outside the callback.

The S3 receive descriptor has a 12-bit `sig_len`, including FCS. Reserve capacity
for up to 4095 delivered bytes, but distinguish reported length, copied length
and metadata-only reports. `WIFI_PKT_MISC` has no payload to copy even if its
metadata reports a length. Test FCS behavior and aggregation before defining
packet export semantics. Keep the 32-bit hardware timestamp separately from the
shared 64-bit `esp_timer` callback timestamp; never mistake the former for a
reboot-independent clock. See the [native receive types](https://github.com/espressif/esp-idf/blob/v5.5.3/components/esp_wifi/include/local/esp_wifi_types_native.h).

Use the full 4095-byte driver-length ceiling as raw mode's default snap length.
Never silently truncate. A configured shorter snap length must be recorded as
truncation; pool exhaustion drops the whole admitted frame and increments its
own counter. Retain the current BLE payload limit unless a separate, tested
public-API change expands it.

### Records and crash recovery

Keep the v1 `BLER` envelope, CRC, 1,024-byte maximum, partition and per-record
two-phase commit. Add record types, leaving existing types 0x01–0x04 unchanged:

| Proposed type | Purpose |
|---|---|
| 0x05 CaptureStart | Session/boot IDs, clock mapping, firmware, both-radio settings, country/channel list, filters, snap length, schedule/seed |
| 0x06 WifiFrameFragment | Session/frame sequence, callback time, window ID, public RX metadata, lengths, flags, fragment offset and raw bytes |
| 0x07 RadioWindow | Begin/end event, window ID, radio/channel, monotonic time, requested settings and switch outcome |
| 0x08 CaptureStats | Per-radio admitted/stored/drop counters, pool/queue peaks, bytes and timing statistics |
| 0x09 CaptureEnd | Final counters, stop reason and end time |

For new sessions use CaptureStart/End even in dedicated modes. Continue using
the existing BleObservation and ClockSync encodings; the upgraded host handles
both legacy and new session records. Assign BLE and Wi-Fi independent observation
sequences and identify the radio explicitly; file order need not be timestamp
order when draining two queues.

Encode Wi-Fi frames as one or more records with at most 896 payload bytes each,
leaving header space inside 1,024 bytes. Exact layouts, sentinels and invariants
must be specified in FORMAT.md before coding. Preflight space for all fragments
and the enlarged final-record reserve before committing the first fragment.
Commit each fragment individually; count a frame stored only when complete.

After power loss, earlier committed records remain valid. A partially programmed
record still yields CORRUPT_TAIL. A power cut between committed fragments may
leave a clean record prefix with an incomplete logical Wi-Fi frame: recovery
preserves it, export reports it explicitly and never treats it as a complete
frame. Never join fragments across sessions or inconsistent metadata. Structural
record validation and logical frame completeness are separate results.

Old v1 decoders can skip the new types by length/CRC, but cannot interpret new
session metadata or export Wi-Fi. Preserve old-file support, require an upgraded
CLI for complete new exports, and report unknown/skipped types. Downgrading
firmware is not a supported way to operate a mixed log.

## Storage duration and field behavior

The current log partition is 13,565,952 bytes (about 12.94 MiB). Illustrative
capacity calculations, using total encoded bytes across both radios:

| Average encoded rate | Approximate time to FULL |
|---:|---:|
| 10,000 bytes/s | 22.6 minutes |
| 50,000 bytes/s | 4.5 minutes |
| 100,000 bytes/s | 2.3 minutes |

For one hour the total budget is only about 3,768 bytes/s; for two hours about
1,884 bytes/s. These calculations are capacity limits, not measured traffic
rates. Include fragmentation, window records and final reserve in estimates.
Measure flash commit throughput separately; buffer size cannot fix sustained
input exceeding write throughput.

Expose bytes/second and estimated remaining duration in STATS/watch. Never
silently switch filters, deduplicate, overwrite old data or claim lossless raw
capture. If the required outdoor duration exceeds flash capacity, choose an
explicit summary/filter mode or add microSD as a subsequent hardware/storage
project. PSRAM is volatile and does not solve standalone persistent capacity.

Persist the selected boot capture profile, country and timing in a versioned NVS
configuration only on an explicit save operation. On a clean first boot use
balanced raw capture with world-safe channels; report any invalid saved config
and its fallback. Profile persistence is necessary for the existing workflow
of configuring on a PC, unplugging, then booting from a powerbank.

Wall-clock synchronization still does not survive power loss. Reuse the existing
per-session mapping rules and export unknown absolute time as null. Recover and
autostart only with a clean tail and enough room. Download remains possible for
a corrupt tail; erase is explicit and never part of pull.

## USB protocol and CLI

Keep the current COBS framing, status codes and dump protocol. Existing INFO,
STATS and START payload lengths are exact, so do not append fields in place.
Add capability negotiation and new versioned commands for capture configuration,
extended info/stats, and explicit boot-profile save/read. Old START_CAPTURE stays
BLE-only. New CLI commands fall back for old devices where meaningful, and
reject unsupported options before capture starts.

Proposed command interface (not executable yet):

```sh
blelog-cli start --port /dev/ttyACM0 --radios both --profile raw --timing balanced
blelog-cli config save --port /dev/ttyACM0 --radios both --profile raw --timing balanced --country FI
blelog-cli watch --port /dev/ttyACM0 --interval 1
blelog-cli pull --port /dev/ttyACM0 --output walk.blelog
blelog-cli export jsonl walk.blelog -o walk.jsonl
blelog-cli export csv walk.blelog --radio wifi -o walk-wifi.csv
blelog-cli export pcapng walk.blelog --radio wifi -o walk-wifi.pcapng
```

Use the country where the device is operated; FI above is an example. Saving
configuration requires stopped capture. `prepare` still confirms erase and can
optionally save the selected boot profile. `watch` uses one persistent serial
connection and shows radio/channel, measured listening time, rates, drops, pool
use, storage and errors. Live packet streaming is a separate future feature.

Retain `.blelog` as the canonical mixed archive. JSONL uses typed rows. CSV offers
separate BLE/Wi-Fi schemas. Wi-Fi host parsing derives frame type/subtype,
addresses (with correct ToDS/FromDS and short-control-frame handling), SSID bytes,
channel and security information when present. Hidden/non-UTF-8 SSIDs, randomized
MAC addresses, malformed/truncated frames and unavailable fields must not become
invented identities or values. Unique MAC counts are not counts of people.

PCAPNG initially exports reconstructable Wi-Fi frames only, with appropriate
802.11/radio metadata and FCS/captured/original-length treatment. Do not fabricate
BLE link-layer frames from HCI advertising reports. For sessions without wall
time, explicitly label any relative/synthetic PCAPNG timeline and retain the
canonical monotonic timestamps. The verifier reports record counts, complete
frames, incomplete frames and per-radio export counts separately.

## Implementation sequence and acceptance gates

1. **Baseline and contracts:** preserve existing captures; document new layouts,
   protocol capabilities and settings. Add golden vectors, old-file compatibility,
   fragment-boundary/malformed-length tests, unknown types, and crash injection
   during every fragment body/commit and between fragments. No automatic erase.
2. **Wi-Fi-only receiver:** implement fixed-channel capture through the existing
   writer. Compare known beacons, probes, action, short control, encrypted data,
   large frames, aggregation and FCS against a reference capture. Verify byte
   equality where the driver exposes bytes. Record unsupported cases honestly.
3. **Scheduler:** integrate bounded BLE windows and channel hopping. Fake-clock
   tests prove channel fairness, maximum gaps and STOP/FULL/error handling at
   every transition. Test late callbacks, queue saturation, repeated starts,
   final reserve and no deadlock. Driver failures must leave USB diagnostics.
4. **Host workflow:** negotiate old/new firmware, pull/resume mixed logs, export
   JSONL/CSV/PCAPNG and verify observation-count equality after fragment assembly.
   Test saved boot profile, clock loss after reboot and live watch responsiveness.
5. **Radio tuning:** compare BLE-only, fixed-channel Wi-Fi-only, hopping Wi-Fi-only,
   explicit scheduling, and automatic coexistence. Sweep BLE windows 0.5/1/2 s,
   Wi-Fi dwells 120/200/300 ms and the three timing profiles. Use controlled BLE
   advertisers with 100 ms/1 s/2 s intervals, active/passive and extended reports,
   plus known Wi-Fi sources on several channels. Randomize test order and repeat.
6. **Measure the real objective:** retained raw frames/bytes per radio, both per
   elapsed second and per actual listening second; discovery recall after 5/10/30 seconds,
   time-to-first-detection percentiles, per-radio/channel listening duty and blind
   gaps, switch latency, admitted-versus-stored reports, queue/pool drops, flash
   write latency, bytes/s and current consumption. Software counters cannot count
   packets never delivered by the radio; use reference receivers/transmitter
   counters for reception loss. Choose defaults from these results, not nominal
   duty percentages alone.
7. **Field qualification:** verify PSRAM-absent operation, app still fitting the
   3 MiB partition, reset/power-cut recovery, full flash without overwrite,
   sustained burst overload, USB/powerbank use and measured-duration outdoor runs.
   Gate a 1–2 hour raw soak on measured storage capacity or an added storage sink;
   otherwise separately qualify time-to-FULL and repeated shorter captures.

Keep ESP-IDF 5.5.3 and esp32-nimble 0.12.0 initially. Run portable tests/clippy and
the firmware cross-build, then hardware gates. This planning change requires no
firmware flash or log erase.

## Decisions to confirm through use and measurements

The user selected maximum raw content and also endorsed keeping both capture
profiles. Therefore raw is the default and discovery is an explicit alternative;
switching profiles must never happen silently. There is no universally best
interval without the target
transmit patterns and desired recording duration. Start with balanced scheduling
and measure before changing it adaptively. If missing brief events is unacceptable,
two boards (one dedicated to BLE, one to Wi-Fi) remove cross-protocol blind windows;
the Wi-Fi board still samples only its tuned channel configuration at a time.
