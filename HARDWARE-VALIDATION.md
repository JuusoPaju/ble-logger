# Dual-radio validation — 2026-09-12

## Confirmed

- ESP32-S3 revision 0.2; physical flash 16 MiB, octal PSRAM 8 MiB.
- Dedicated log partition: offset `0x310000`, size `0xcf0000` (13,565,952 bytes).
- Pre-upgrade backup `/tmp/blelog-before-wifi-20260912.blelog`: 1,191,448
  bytes, 15,284 valid records, 15,281 BLE observations. The v0.2 CLI verifies it.
- Firmware application links and fits the 3 MiB application slot. Only the
  application region was flashed during startup debugging; the log was not erased.
- UART/COM bridge enumerates as `1a86:55d3`, `/dev/ttyACM0` in this VM.
  Native USB is a different device (`303a:1001`); the pathname alone is insufficient
  to identify the correct control/download port.
- Latest boot recovered 1,211,424 bytes / 15,577 records (including short diagnostic
  sessions), initialized both drivers and entered the alternating scheduler.
  UART shows repeated extended BLE discovery followed by five Wi-Fi promiscuous
  dwells and clean Wi-Fi shutdown between BLE windows, without the earlier crashes.
- All 22 host tests pass; host clippy with `-D warnings`, formatting and diff
  whitespace checks pass.

## Startup fixes verified through UART

1. Split internal FreeRTOS queues into small allocations, maintaining FIFO order
   and nonblocking admission. Capacities are 128 BLE observations / 8 full Wi-Fi
   frames; the original large allocations exhausted internal memory.
2. Use a 24 KiB storage stack and release a separate temporary 32 KiB internal
   recovery buffer before initializing the radios.
3. Reduce unused connection-oriented NimBLE buffer pools: MSYS1 count 12,
   transport ACL count 4, high-priority event count 8. Advertising-event pool
   and extended event size remain unchanged. Boot reported 19,159 internal bytes
   free after autostart; this is not a peak-load memory qualification.
4. Explicitly align the internal Wi-Fi scratch allocation to `QueuedWifi`'s Rust
   alignment (8 bytes); the default allocator's 4-byte alignment caused a panic
   on the first Wi-Fi packet.
5. Remove temporary raw USB startup diagnostics; UART alone carries text logs.
   Refuse NVS initialization errors before the NimBLE wrapper can erase NVS.

## Still required

- Extend the passing short lifecycle test below to dense traffic and long soak.
- Qualify saved boot configuration across reboot.
- Qualify callback payload preservation against a reference capture, long Wi-Fi
  frames/reassembly, failed-FCS delivery, extended BLE payloads and passive BLE.
- Measure saturation, queue drops, memory/stack headroom, channel revisit times,
  repeated radio switching, FULL behavior, injected power loss, dense-environment
  soak and battery-powered field use. These are not implied by passing host tests
  or by a short successful boot trace. PSRAM-failure operation is not yet tested.

## Native-USB results and remaining runtime issue

Native USB `303a:1001` reconnects and INFO/STATS/watch/pull work. First mixed
download `/tmp/blelog-dual-radio-20260912.blelog` contains 1,776,072 bytes,
19,041 records, 16,148 BLE observations and 1,958 complete Wi-Fi frames, with
zero incomplete frames. Original 1,191,448-byte prefix matches the backup exactly.
JSONL exports 18,106 rows; separate CSV exports have 16,148 BLE and 1,958 Wi-Fi
rows. All 1,958 PCAPNG packet byte sequences match their JSONL payloads, and PCAPNG
block lengths/trailers validate. Maximum observed Wi-Fi length was only 569 bytes;
large-fragment hardware behavior remains unqualified. None of these 1,958 stored
trailer values matches a software FCS check, despite driver `rx_state=0`.
The archive preserves all driver-delivered bytes; PCAPNG flags checksum mismatch.
Do not equate these mismatches with proven RF corruption or verified on-air FCS.

The latest raw session before the first download reported 707 Wi-Fi callbacks,
684 stored, 23 queue drops, zero flash errors; 263/263 BLE reports stored.
Requested-window totals were 28.52 s BLE and 28.58 s Wi-Fi, 0.07 s switching,
maximum record commit 2,364 us. This is a short sample, not sustained throughput.

Restart initially returned `Io: Ble` because allocating another 16 KiB scanner
stack failed. Firmware now reserves one persistent scanner task before Wi-Fi
initialization and reuses it across sessions. That build linked/flashed and
successfully started discovery after stopping raw. Discovery's verified session
has 162 BLE observations and 227 Wi-Fi frames, all management type, all with
reconstructed Unix time after clock sync; live polling initially showed zero drops.

However, a subsequent STOP timed out and the device returned in unsynchronized
default raw capture. Downloads show intervening new raw sessions, confirming a
reset occurred; its cause is not established without UART logs. Do not mark
repeated stop/start or field reliability complete. Latest preserved archive:
`/tmp/blelog-dual-profile-20260912.blelog`, 2,351,332 bytes / 22,785 records /
16,886 BLE observations / 3,956 complete Wi-Fi frames / zero incomplete frames.
Its JSONL has 20,842 rows. The original backup prefix still matches byte-for-byte.
The device was left stopped after this download; no log erase was performed.

## Reconnection clarification and recovery boundary

The user subsequently confirmed reconnecting the board during the previous test.
That can explain the timeout and new raw sessions. The earlier reset observations
are therefore confounded, not a demonstrated firmware stop/profile-switch bug.

A default-off `radio-selftest` feature now provides a reproducible single-cable
UART test: 20 starts alternating discovery/raw, varied stop timing, 40 total stop
calls including idempotency checks, then stopped. It neither erases nor changes
saved boot settings. The first attempt did not run: capture start was refused
because the recovered log was not appendable.

UART recovery found 2,827,032 committed bytes / 25,853 records. A read-only UART
flash dump to `/tmp/blelog-reconnected-prefix-20260912.blelog` verifies as 17,612
BLE observations / 5,677 complete Wi-Fi frames / zero incomplete frames. Its
2,351,332-byte prefix matches the previous download exactly. SHA-256:
`2ab2a37bc12dc4edabe04562f0efda4098d5d7d61186a9539c2d92a29521104b`.

The 128-byte boundary dump `/tmp/blelog-reconnected-tail-20260912.bin`, read from
physical address `0x5c2318`, starts `ff ff ff ff 01 06 0e 00 5c 00 00 00`:
an uncommitted, partly programmed Wi-Fi fragment body, with no BLER marker.
This is consistent with interrupted flash programming. It does not establish
which particular disconnection/reset interrupted it. Correct recovery retains
all earlier committed records and disables append (CORRUPT_TAIL).

Normal firmware was restored and UART confirms `CorruptTail`; no erase has been
authorized or performed. Verified durable copies are now in the repository's
ignored `captures/recovered-20260912.blelog` and `captures/recovered-20260912-tail.bin`.
At that point, the lifecycle test remained unrun pending explicit erase approval.
The copies in `captures/` survive host temporary-file cleanup;
keep them before deleting any device data.

## Approved erase and controlled lifecycle result

The user explicitly authorized clearing the on-device log. Erased only
`0x310000..0x1000000`; firmware and NVS were not erased. Recovery then confirmed
zero bytes and state Ready, including verification that the remaining partition
was erased. Earlier data is preserved in `captures/recovered-20260912.blelog`.

With the cable left connected, the first controlled run reproduced a stack
overflow during session 14 (`***ERROR*** A stack overflow in task pthread has
been detected`, followed by reset reason 4). Thus a separate firmware fault was
demonstrated; the user's earlier reconnection still confounds that earlier timeout.

Fixed the storage receive/drain path: instead of returning a 4 KiB `QueuedWifi`
by value through several stack temporaries, receive directly into one reusable,
properly aligned internal-RAM allocation. Copy-only queues also avoid pointless
large-value draining during destruction. The storage stack stays at 24 KiB.

The revised build passed all 20 alternating discovery/raw starts and 40 STOP
calls, ending every session in Ready with no reset, Wi-Fi queue drop or scheduler
failure. Observed minimum remaining stack: storage 19,852 bytes, scanner 13,052
bytes; observed stop durations 25–960 ms (including the idempotent STOP).

Saved the 250,468-byte test prefix in `captures/lifecycle-20260912.blelog`:
2,272 records, 1,218 BLE observations and 398 complete Wi-Fi frames, zero
incomplete frames (includes earlier diagnostic sessions). JSONL has 1,616 rows.
The final 20 successful sessions have 352 BLE observations and 78 Wi-Fi frames.
All final 20 SessionEnd records are present, profiles alternate as requested,
Wi-Fi row counts match stored session counters, and every discovery Wi-Fi frame
is management type. All 22 host tests and strict host clippy still pass.

Normal firmware was successfully flashed without `radio-selftest`, leaving the board in
the bootloader deliberately: moving the sole cable back to native USB will then
power-cycle into normal capture without first cutting power during active logging.
Fresh test records remain on-device; the original pre-erase data is only in backups.

## Final native-USB check after overflow fix

The user moved the cable back to native USB (`303a:1001`). Normal firmware 0.2.0
autostarted Raw/Both and responded to INFO. STOP succeeded: initial live session
stored 156/156 BLE reports and 301/314 Wi-Fi reports, with 13 Wi-Fi queue drops,
zero flash errors and zero scheduler failures. This confirms raw mode can still
drop bursts despite the stack fix; do not claim lossless RF capture.

Clock sync, discovery START, live watch, STOP, USB pull, and raw START then all
completed successfully without a reset. Discovery stored all 154 Wi-Fi reports
with zero queue drops or errors. The final USB archive
`captures/native-usb-verified-20260912.blelog` verifies at 411,000 bytes / 3,325
records / 1,506 BLE observations / 853 complete Wi-Fi frames / zero incomplete
frames. Its first 250,468 bytes match the UART lifecycle backup exactly.

Final INFO reports Capturing, Raw/Both, active BLE 100/100 ms, alternating
1,000 ms BLE / five 200 ms Wi-Fi dwells, world-safe channels 1–11, and
13,154,788 bytes free at that snapshot. The board is intentionally left recording.

## RGB LED pin probe (2026-09-12)

The user's board photos in `boardpics/` show a square LED next to an `RGB`
silkscreen label, but do not conclusively identify its GPIO. A standalone,
feature-gated `rgb-probe` binary tests only GPIO48 and GPIO38 using hardware RMT:
one white flash on GPIO48, then two blue flashes on GPIO38, repeating every five
seconds. It does not initialize either radio, NVS, storage, or the USB CLI.
Do not connect external circuits to those pins during this probe.

Capture was stopped successfully over native USB before flashing. INFO reported
Ready, 4,335,160 valid bytes, 29,336 records and 9,230,792 free bytes. A verified
backup is `captures/before-rgb-probe-20260912.blelog`: 7,335 BLE observations,
14,947 Wi-Fi frames, zero incomplete frames.

Build: `cargo +esp firmware-build --offline --features rgb-probe --bin rgb-probe`.
The linked image was converted with the pinned esptool, using QIO / 80 MHz /
16 MiB. The 661,216-byte `rgb-probe.bin` was successfully flashed at `0x10000`
only. No log, partition-table, bootloader, or NVS erase was performed. Formatting
and diff whitespace checks passed. LED operation and the correct pin are pending
the user's visual confirmation; a successful flash is not proof of LED operation.

The device now has temporary probe firmware, **not a running logger**. Restore
the normal `ble-logger-app.bin` to `0x10000` after observing the LED, then verify
INFO and the recovered prefix. The normal application artifact was preserved.

### Visual pin confirmation

The user supplied `boardpics/IMG_4530.mov` (10.33 seconds). Frame inspection at
20 fps around the first flash shows a single uninterrupted approximately 800 ms
pulse, repeated about five seconds later. This matches the GPIO48 probe, not
GPIO38's two 300 ms pulses separated by 300 ms darkness. The camera renders the
white pulse with a blue/violet cast, so timing, rather than apparent colour, is
the decisive evidence. **GPIO48 is the onboard RGB LED control pin on this board.**
Individual red/green/blue channel order and the final status-indicator integration
remain untested.

Normal firmware was subsequently restored successfully at `0x10000`. INFO
confirmed 0.2.0 Capturing / Raw / Both with 16 MiB flash and 8 MiB PSRAM. A clean
STOP and pull produced `captures/after-rgb-probe-20260912.blelog`: 4,410,604 bytes,
29,806 records, 7,480 BLE observations, 15,159 Wi-Fi frames, zero incomplete.
`cmp -n 4335160` against the pre-probe backup passed: every pre-test byte is
preserved. Raw/Both capture was restarted and INFO confirmed Capturing with
9,155,184 bytes free. The probe is no longer running; status LED integration is
not yet implemented.
