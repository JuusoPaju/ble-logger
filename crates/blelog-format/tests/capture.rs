use blelog_format::{
    capture::*,
    record::{StreamTail, MAX_RECORD_LEN},
    *,
};

fn image(record: CaptureRecord<'_>) -> Vec<u8> {
    let mut buffer = [0u8; MAX_RECORD_LEN];
    let n = encode_record(&Record::Capture(record), &mut buffer).unwrap();
    buffer[..n].to_vec()
}
fn meta(len: usize) -> WifiMeta {
    WifiMeta {
        session_id: 0x12345678,
        sequence: 17,
        monotonic_us: 123456789,
        window_id: 9,
        hardware_timestamp_us: 9876,
        original_len: len as u16,
        captured_len: len as u16,
        channel: 11,
        rssi_dbm: -63,
        flags: if len == 0 {
            WIFI_METADATA_ONLY
        } else {
            WIFI_FCS_INCLUDED
        },
        ..WifiMeta::default()
    }
}
#[test]
fn config_wire_vector_and_validation() {
    let config = CaptureConfig::default();
    let mut bytes = [0u8; CONFIG_LEN];
    config.encode(&mut bytes).unwrap();
    assert_eq!(
        bytes,
        [1, 1, 3, 1, 100, 0, 100, 0, 232, 3, 200, 0, 5, 0, 0, b'0', b'1', 0, 0, 0]
    );
    assert_eq!(CaptureConfig::decode(&bytes).unwrap(), config);
    for n in 0..CONFIG_LEN {
        assert!(CaptureConfig::decode(&bytes[..n]).is_err());
    }
    for index in [0, 1, 2, 3, 12, 17, 18, 19] {
        let mut bad = bytes;
        bad[index] = 255;
        assert!(CaptureConfig::decode(&bad).is_err());
    }
    assert!(CaptureConfig {
        ble_ms: 0,
        ..config
    }
    .validate()
    .is_err());
    assert!(CaptureConfig {
        wifi_dwell_ms: 1001,
        ..config
    }
    .validate()
    .is_err());
    assert!(CaptureConfig {
        channel_mask: 1 << 13,
        ..config
    }
    .validate()
    .is_err());
}
#[test]
fn all_extension_records_round_trip_and_bounds() {
    let records = [
        CaptureRecord::Config {
            session_id: 7,
            seed: 42,
            config: CaptureConfig::default(),
        },
        CaptureRecord::Window {
            session_id: 7,
            window_id: 1,
            monotonic_us: 300,
            radio: 2,
            channel: 13,
            begin: true,
        },
        CaptureRecord::Stats {
            session_id: 7,
            stats: RadioStats([u64::MAX; RADIO_STATS_FIELDS]),
        },
    ];
    for record in records {
        let bytes = image(record);
        assert_eq!(
            decode_record(&bytes).unwrap().record,
            DecodedRecord::Capture(record)
        );
    }
    for n in [0, 31, 255, 896, 897, 1500, 4095] {
        let data: Vec<u8> = (0..n).map(|i| (i % 251) as u8).collect();
        for start in (0..n.max(1)).step_by(FRAGMENT_LEN) {
            let f = WifiFragment {
                meta: meta(n),
                offset: start as u16,
                bytes: &data[start..(start + FRAGMENT_LEN).min(n)],
            };
            let bytes = image(CaptureRecord::Wifi(f));
            assert!(bytes.len() <= MAX_RECORD_LEN);
            assert_eq!(bytes.len() % 4, 0);
            assert_eq!(
                decode_record(&bytes).unwrap().record,
                DecodedRecord::Capture(CaptureRecord::Wifi(f))
            );
            for i in 0..bytes.len() {
                let mut bad = bytes.clone();
                bad[i] ^= 1;
                assert!(decode_record(&bad).is_err());
            }
        }
    }
    assert!(WifiFragment {
        meta: meta(100),
        offset: 99,
        bytes: &[0, 0]
    }
    .validate()
    .is_err());
    assert!(WifiFragment {
        meta: meta(4096),
        offset: 0,
        bytes: &[0; FRAGMENT_LEN]
    }
    .validate()
    .is_err());
}
#[test]
fn every_fragment_programming_cut_preserves_previous_records() {
    let first = image(CaptureRecord::Config {
        session_id: 7,
        seed: 42,
        config: CaptureConfig::default(),
    });
    let data = [0xA7; FRAGMENT_LEN];
    let next = image(CaptureRecord::Wifi(WifiFragment {
        meta: meta(4095),
        offset: 0,
        bytes: &data,
    }));
    for committed in 0..=next.len() {
        let mut partition = first.clone();
        partition.extend(vec![0xff; next.len() + 1024]);
        let start = first.len();
        let body = (next.len() - 4).min(committed);
        partition[start + 4..start + 4 + body].copy_from_slice(&next[4..4 + body]);
        if committed > next.len() - 4 {
            let magic = committed - (next.len() - 4);
            partition[start..start + magic].copy_from_slice(&next[..magic]);
        }
        let scan = scan_stream(&partition);
        assert_eq!(
            scan.valid_bytes,
            first.len()
                + if committed == next.len() {
                    next.len()
                } else {
                    0
                }
        );
        if committed != 0 && committed != next.len() {
            assert!(matches!(scan.tail, StreamTail::Corrupt(_)));
        }
    }
}
#[test]
fn channel_cursor_is_fair_across_arbitrary_burst_boundaries() {
    for mask in [1, 0x1fff, 0x421] {
        for seed in 0..30 {
            let mut cursor = ChannelCursor::new(mask, seed).unwrap();
            let mut counts = [0; 13];
            for _ in 0..mask.count_ones() * 10 {
                counts[cursor.next_channel() as usize - 1] += 1;
            }
            for (i, count) in counts.iter().enumerate() {
                assert_eq!(*count, if mask & (1 << i) != 0 { 10 } else { 0 });
            }
        }
    }
    assert!(ChannelCursor::new(0, 0).is_err());
}
