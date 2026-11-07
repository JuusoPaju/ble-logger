use blelog_cli::{
    export::{export_file, export_filtered, ExportFormat},
    verify::{analyze_wifi, verify_bytes},
};
use blelog_format::{capture::*, record::MAX_RECORD_LEN, *};
use std::fs;

fn append(log: &mut Vec<u8>, record: Record<'_>) {
    let mut out = [0; MAX_RECORD_LEN];
    let n = encode_record(&record, &mut out).unwrap();
    log.extend_from_slice(&out[..n]);
}
fn wifi_log() -> (Vec<u8>, Vec<u8>) {
    let mut data = vec![0xAA; 1800];
    data[..2].copy_from_slice(&[0x80, 0]);
    data[36..42].copy_from_slice(&[0, 4, b't', b'e', b's', b't']);
    let meta = WifiMeta {
        session_id: 7,
        sequence: 3,
        monotonic_us: 9999,
        channel: 6,
        original_len: 1800,
        captured_len: 1800,
        rssi_dbm: -50,
        ..WifiMeta::default()
    };
    let mut log = Vec::new();
    for (i, bytes) in data.chunks(FRAGMENT_LEN).enumerate() {
        append(
            &mut log,
            Record::Capture(CaptureRecord::Wifi(WifiFragment {
                meta,
                offset: (i * FRAGMENT_LEN) as u16,
                bytes,
            })),
        );
    }
    (log, data)
}
#[test]
fn raw_wifi_round_trip_exports_and_pcap_block_integrity() {
    let (log, data) = wifi_log();
    verify_bytes(&log, false).unwrap();
    assert_eq!(analyze_wifi(&log).unwrap().frames, 1);
    let tmp = tempfile::tempdir().unwrap();
    let input = tmp.path().join("input.blelog");
    fs::write(&input, &log).unwrap();
    for (name, format) in [
        ("out.jsonl", ExportFormat::Jsonl),
        ("out.csv", ExportFormat::Csv),
        ("out.pcapng", ExportFormat::Pcapng),
    ] {
        let output = tmp.path().join(name);
        assert_eq!(export_file(&input, &output, format).unwrap(), 1);
        let out = fs::read(output).unwrap();
        if name.ends_with("jsonl") {
            let row: serde_json::Value = serde_json::from_slice(&out).unwrap();
            assert_eq!(row["radio"], "wifi");
            assert_eq!(row["ssid"], "test");
            assert!(row["unix_timestamp_ms"].is_null());
            assert_eq!(row["raw_payload_hex"].as_str().unwrap().len(), 3600);
        }
        if name.ends_with("pcapng") {
            let mut offset = 0;
            let mut packets = 0;
            while offset < out.len() {
                let kind = u32::from_le_bytes(out[offset..offset + 4].try_into().unwrap());
                let len =
                    u32::from_le_bytes(out[offset + 4..offset + 8].try_into().unwrap()) as usize;
                assert!(len >= 12 && len % 4 == 0);
                assert_eq!(
                    u32::from_le_bytes(out[offset + len - 4..offset + len].try_into().unwrap())
                        as usize,
                    len
                );
                if kind == 6 {
                    packets += 1;
                    assert_eq!(
                        &out[offset + 28 + 15..offset + 28 + 15 + data.len()],
                        data.as_slice()
                    );
                }
                offset += len;
            }
            assert_eq!(packets, 1);
        }
    }
    assert!(export_file(&input, &input, ExportFormat::Jsonl).is_err());
    assert_eq!(fs::read(input).unwrap(), log);
}
#[test]
fn incomplete_frame_remains_valid_archive_but_is_not_a_complete_packet() {
    let (log, _) = wifi_log();
    let first = decode_record(&log).unwrap().total_len;
    let prefix = &log[..first];
    verify_bytes(prefix, false).unwrap();
    let summary = analyze_wifi(prefix).unwrap();
    assert_eq!(summary.frames, 0);
    assert_eq!(summary.incomplete, 1);
    assert!(analyze_wifi(&log[first..]).is_err());
    let tmp = tempfile::tempdir().unwrap();
    let input = tmp.path().join("cut.blelog");
    fs::write(&input, prefix).unwrap();
    assert_eq!(
        export_filtered(
            &input,
            &tmp.path().join("cut.jsonl"),
            ExportFormat::Jsonl,
            Radios::Wifi
        )
        .unwrap(),
        0
    );
}
