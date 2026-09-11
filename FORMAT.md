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

## USB frames

Decoded frames contain version `u8`, message type `u8`, request ID `u16`, payload
length `u32`, payload bytes, and CRC-32. They are COBS-encoded and terminated by
`00`. Response message types set bit 7 and response payloads begin with a stable
little-endian `u16` status code. Dump chunks are at most 4096 bytes and are
protected by the outer frame CRC.
