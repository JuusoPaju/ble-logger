# BLE Logger formats, version 1

All numeric values are little-endian. Advertiser addresses are the six bytes in
canonical display order. Every flash record is four-byte aligned, at most 1024
bytes, and ends with CRC-32/ISO-HDLC over all preceding bytes (including zero
padding). The check value for `123456789` is `CBF43926`.

## Record envelope

| Offset | Size | Field |
|---:|---:|---|
| 0 | 4 | `BLER` commit marker |
| 4 | 1 | format version (`1`) |
| 5 | 1 | record type |
| 6 | 2 | absolute payload offset (`header_len`) |
| 8 | 4 | aligned total record length, including CRC |
| 12 | variable | type-specific header |
| `header_len` | variable | payload |
| variable | 0–3 | zero padding |
| `total_len - 4` | 4 | CRC-32/ISO-HDLC |

Firmware programs bytes 4 through the CRC first and programs `BLER` last. A v1
decoder validates and skips unknown record types by total length. Any unsupported
version stops recovery.

Record types and header lengths are:

- `01` SessionStart, 56 bytes: session ID, boot ID, start monotonic time, optional
  Unix/monotonic mapping, scan mode/settings, extended-advertising flag, then a
  length-prefixed firmware version payload.
- `02` BleObservation, 54 bytes: session/sequence, monotonic time, address/type,
  RSSI, report kind, data status, PHYs, SID, Tx power, legacy event type, extended
  properties, periodic interval, and `u16` raw-payload length.
- `03` SessionEnd, 68 bytes: session/stop time and received, stored, queue-drop,
  storage-full-drop, and flash-error counters.
- `04` ClockSync, 36 bytes: session ID and the exact Unix-ms/monotonic-us pair.

Unavailable metadata uses `FF`, `FFFF`, or Tx power `7F`; unknown wall time is
signed `-1`.

## Additive dual-radio records (firmware 0.2)

The envelope/version/1024-byte limit do not change. SessionStart, BleObservation,
ClockSync and SessionEnd retain their old layouts. A SessionStart is followed by
CaptureConfig before either producer starts. SessionEnd's observation counters
remain BLE-only; RadioStats immediately before it contains Wi-Fi/session timing
counters. This avoids changing the meaning of old records. Old tools skip the
extensions but cannot export Wi-Fi; new tools read both generations.

Types 05–08 all have `header_len=14`; bytes 12–13 are the exact little-endian body
length. The body is followed by zero alignment padding and the envelope CRC.

| Type | Body, in byte order |
|---|---|
| 05 CaptureConfig | session:u64, schedule_seed:u32, config:20 bytes |
| 06 WifiFragment | metadata below:43 bytes, plus fragment_offset:u16 inserted at offset 32, then up to 896 frame bytes (45-byte fixed body) |
| 07 RadioWindow | session:u64, window:u32, monotonic_us:u64, radio:u8 (1 BLE / 2 Wi-Fi), channel:u8 (0 for BLE), begin:u8 |
| 08 RadioStats | session:u64, twelve u64 counters below |

WifiFragment body offsets: session 0, sequence 8, callback monotonic_us 12,
window ID 20, hardware timestamp_us 24, original_len:u16 28, captured_len:u16 30,
fragment_offset:u16 32; byte fields at 34–44 are channel, secondary_channel,
RSSI (i8), driver packet type, rate, sig_mode, MCS, bandwidth, aggregation,
rx_state and flags. Raw bytes start at body offset 45. Flags: bit 0 metadata-only,
bit 1 FCS included. Undefined flags are rejected. Metadata-only records have
captured_len=0. Driver length is bounded to 4095 bytes; no default truncation.

Fragments identify one frame by session/sequence, repeat identical metadata and
have offsets 0,896,1792,... with only the last fragment shorter. The writer checks
space for the whole frame plus 196 bytes for RadioStats/SessionEnd before the
first fragment. Every fragment has its own two-phase commit. After a power cut,
an incomplete logical frame can remain in an otherwise valid record prefix;
count it separately and never export it as a complete frame. No cross-session
reassembly is allowed. Zero queue drops does not imply zero RF packet loss.

RadioStats counters: Wi-Fi received, stored, queue drops, full drops, error drops,
invalid reports, Wi-Fi queue peak, BLE window microseconds, Wi-Fi window
microseconds, switching microseconds, scheduler failures, maximum flash commit
microseconds. They reset per session (unlike the legacy boot-scoped STATS).
Firmware internally accumulates timing in milliseconds to avoid 32-bit
microsecond overflow; exported timing consequently has millisecond resolution.
Window markers timestamp software start/end requests, not precise hardware RF
arbitration. A terminal stop/full/error may leave the last window without an end
marker; SessionEnd and final counters delimit the capture when available.

Config bytes: schema=1 at 0; profile (0 discovery,1 raw) at 1; radios (1 BLE,2
Wi-Fi,3 both) at 2; the legacy five-byte StartCapture at 3–7; BLE window duration
u16 at 8; Wi-Fi dwell u16 at 10; channels-per-burst at 12; channel mask:u16 at 13
(bit 0 = channel 1, zero = country defaults); two-byte country at 15; failed-FCS
boolean at 17; reserved zero bytes 18–19. BLE duration is 100–2000 ms in 10 ms
steps, Wi-Fi dwell 50–1000 ms, burst 1–13 channels. The resolved channel mask is
recorded in the session. Seed selects initial channel and dwell jitter; the
cursor advances cyclically through enabled channels without resetting per burst.

## USB frames

Decoded frames contain version `u8`, message type `u8`, request ID `u16`, payload
length `u32`, payload bytes, and CRC-32. They are COBS-encoded and terminated by
`00`. Response message types set bit 7 and response payloads begin with a stable
little-endian `u16` status code. Dump chunks are at most 4096 bytes and are
protected by the outer frame CRC.

Additional commands: 0B CAPABILITIES (empty request, `DUAL1` detail), 0C
START_RADIOS (20-byte config), 0D RADIO_INFO (empty → current 20-byte config), 0E
RADIO_STATS (empty → 96 counter bytes followed by active radio/channel bytes),
0F SAVE_CONFIG (20-byte config, stopped in Ready/Full/CorruptTail), 10 BOOT_CONFIG (empty →
saved 20-byte config). Old INFO/STATS/START lengths remain unchanged; legacy START
starts BLE-only. SAVE_CONFIG commits a versioned blob in NVS, never wall time.
Failed-FCS capture is opt-in pending hardware qualification.
