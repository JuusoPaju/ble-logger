use blelog_format::record::*;

fn observation<'a>(payload: &'a [u8]) -> Record<'a> {
    Record::BleObservation(BleObservation {
        session_id: 0x0102_0304_0506_0708,
        sequence: 0x1122_3344,
        monotonic_us: 0x1020_3040_5060_7080,
        address: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
        address_type: 1,
        rssi_dbm: -67,
        report_kind: ReportKind::ScanResponse,
        data_status: DATA_STATUS_UNKNOWN,
        primary_phy: PHY_UNKNOWN,
        secondary_phy: PHY_UNKNOWN,
        advertising_sid: SID_UNKNOWN,
        tx_power_dbm: TX_POWER_UNKNOWN,
        legacy_event_type: 4,
        extended_properties: EXTENDED_PROPERTIES_UNKNOWN,
        periodic_interval: 0,
        payload,
    })
}

fn round_trip(record: Record<'_>) -> Vec<u8> {
    let mut out = [0u8; MAX_RECORD_LEN];
    let len = encode_record(&record, &mut out).unwrap();
    let decoded = decode_record(&out[..len]).unwrap();
    assert_eq!(decoded.total_len, len);
    match (record, decoded.record) {
        (Record::SessionStart(a), DecodedRecord::SessionStart(b)) => assert_eq!(a, b),
        (Record::BleObservation(a), DecodedRecord::BleObservation(b)) => assert_eq!(a, b),
        (Record::SessionEnd(a), DecodedRecord::SessionEnd(b)) => assert_eq!(a, b),
        (Record::ClockSync(a), DecodedRecord::ClockSync(b)) => assert_eq!(a, b),
        pair => panic!("unexpected decoded pair: {pair:?}"),
    }
    out[..len].to_vec()
}

#[test]
fn golden_vectors_for_all_v1_record_types() {
    let start = round_trip(Record::SessionStart(SessionStart {
        session_id: 0x0102_0304_0506_0708,
        boot_id: 0xAABB_CCDD,
        start_monotonic_us: 123_456,
        wall_clock_unix_ms: 1_789_000_000_123,
        clock_monotonic_us: 120_000,
        scan_mode: ScanMode::Active,
        scan_interval_ms: 100,
        scan_window_ms: 100,
        extended_adv_enabled: true,
        firmware_version: b"0.1.0",
    }));
    let observation = round_trip(observation(&[0x02, 0x01, 0x06]));
    let end = round_trip(Record::SessionEnd(SessionEnd {
        session_id: 0x0102_0304_0506_0708,
        stop_monotonic_us: 999_000,
        total_observations: 100,
        stored_observations: 97,
        dropped_queue_full: 2,
        dropped_storage_full: 1,
        flash_write_errors: 0,
    }));
    let clock = round_trip(Record::ClockSync(ClockSync {
        session_id: 0x0102_0304_0506_0708,
        unix_ms: 1_789_000_000_123,
        monotonic_us: 120_000,
    }));

    assert_eq!(hex(&start), "424c455201013800440000000807060504030201ddccbbaa40e20100000000007ba2b588a0010000c0d40100000000000101640064000500302e312e300000001531907d");
    assert_eq!(hex(&observation), "424c455201023600400000000807060504030201443322118070605040302010aabbccddeeff01bd01ffffffff7f0400ffff000003000201060000003bc99308");
    assert_eq!(hex(&end), "424c455201034400480000000807060504030201583e0f0000000000640000000000000061000000000000000200000000000000010000000000000000000000000000004e80f395");
    assert_eq!(
        hex(&clock),
        "424c4552010424002800000008070605040302017ba2b588a0010000c0d4010000000000b08f8c74"
    );
}

#[test]
fn payload_boundaries_and_alignment() {
    for len in [0usize, 31, 32, 255] {
        let payload: Vec<u8> = (0..len).map(|n| n as u8).collect();
        let bytes = round_trip(observation(&payload));
        assert_eq!(bytes.len() & 3, 0);
        let DecodedRecord::BleObservation(decoded) = decode_record(&bytes).unwrap().record else {
            panic!("wrong type")
        };
        assert_eq!(decoded.payload, payload);
    }
    let too_large = vec![0; 256];
    let mut out = [0u8; MAX_RECORD_LEN];
    assert_eq!(
        encode_record(&observation(&too_large), &mut out),
        Err(RecordError::PayloadTooLong)
    );
}

#[test]
fn detects_every_envelope_corruption_class() {
    let original = round_trip(observation(&[1, 2, 3, 4, 5]));

    assert_eq!(decode_record(&original[..11]), Err(RecordError::Truncated));
    let mut damaged = original.clone();
    damaged[0] ^= 1;
    assert_eq!(decode_record(&damaged), Err(RecordError::InvalidMagic));
    let mut damaged = original.clone();
    damaged[4] = 2;
    assert_eq!(
        decode_record(&damaged),
        Err(RecordError::UnsupportedVersion(2))
    );
    let mut damaged = original.clone();
    damaged[6..8].copy_from_slice(&2u16.to_le_bytes());
    assert_eq!(
        decode_record(&damaged),
        Err(RecordError::InvalidHeaderLength)
    );
    let mut damaged = original.clone();
    damaged[8..12].copy_from_slice(&0x404u32.to_le_bytes());
    assert_eq!(decode_record(&damaged), Err(RecordError::RecordTooLong));
    let mut damaged = original.clone();
    damaged[32] ^= 1;
    assert!(matches!(
        decode_record(&damaged),
        Err(RecordError::CrcMismatch { .. })
    ));
    assert_eq!(
        decode_record(&original[..original.len() - 1]),
        Err(RecordError::Truncated)
    );
}

#[test]
fn unknown_v1_type_is_crc_validated_and_skipped() {
    let mut bytes = round_trip(Record::ClockSync(ClockSync {
        session_id: 1,
        unix_ms: 2,
        monotonic_us: 3,
    }));
    bytes[5] = 0x77;
    let crc_offset = bytes.len() - CRC_LEN;
    let crc = blelog_format::crc::crc32(&bytes[..crc_offset]);
    bytes[crc_offset..].copy_from_slice(&crc.to_le_bytes());
    assert!(matches!(
        decode_record(&bytes).unwrap().record,
        DecodedRecord::Unknown {
            record_type: 0x77,
            ..
        }
    ));

    bytes[20] ^= 1;
    assert!(matches!(
        decode_record(&bytes),
        Err(RecordError::CrcMismatch { .. })
    ));
}

#[test]
fn concatenation_and_erased_tail() {
    let first = round_trip(observation(&[]));
    let second = round_trip(observation(&[1; 31]));
    let mut flash = vec![0xFF; first.len() + second.len() + 4096];
    flash[..first.len()].copy_from_slice(&first);
    flash[first.len()..first.len() + second.len()].copy_from_slice(&second);
    let scan = scan_stream(&flash);
    assert_eq!(scan.valid_bytes, first.len() + second.len());
    assert_eq!(scan.record_count, 2);
    assert_eq!(scan.observation_count, 2);
    assert_eq!(scan.tail, StreamTail::Erased);
}

#[test]
fn two_phase_commit_fault_injection_never_loses_earlier_record() {
    let previous = round_trip(observation(&[0xAA; 31]));
    let next = round_trip(observation(&[0x55; 255]));

    // Body bytes 4..CRC are programmed first. Simulate failure after every byte.
    for programmed in 0..next.len() - 4 {
        let mut flash = vec![0xFF; previous.len() + next.len() + 64];
        flash[..previous.len()].copy_from_slice(&previous);
        let start = previous.len();
        flash[start + 4..start + 4 + programmed].copy_from_slice(&next[4..4 + programmed]);
        let scan = scan_stream(&flash);
        assert_eq!(scan.valid_bytes, previous.len(), "programmed={programmed}");
        assert_eq!(scan.record_count, 1);
        if programmed == 0 {
            assert_eq!(scan.tail, StreamTail::Erased);
        } else {
            assert!(matches!(scan.tail, StreamTail::Corrupt(_)));
        }
    }

    // Then simulate each possible partial commit-marker program.
    for marker_bytes in 0..=4 {
        let mut flash = vec![0xFF; previous.len() + next.len() + 64];
        flash[..previous.len()].copy_from_slice(&previous);
        let start = previous.len();
        flash[start + 4..start + next.len()].copy_from_slice(&next[4..]);
        flash[start..start + marker_bytes].copy_from_slice(&next[..marker_bytes]);
        let scan = scan_stream(&flash);
        if marker_bytes == 4 {
            assert_eq!(scan.valid_bytes, previous.len() + next.len());
            assert_eq!(scan.record_count, 2);
        } else {
            assert_eq!(scan.valid_bytes, previous.len());
            assert!(matches!(scan.tail, StreamTail::Corrupt(_)));
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    use core::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut out, "{byte:02x}").unwrap();
    }
    out
}
