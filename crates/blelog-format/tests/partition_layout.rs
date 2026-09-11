#[test]
fn sixteen_mib_partition_table_is_contiguous_and_bounded() {
    let csv = include_str!("../../../partitions.csv");
    let expected = [
        ("nvs", 0x9000u32, 0x6000u32),
        ("phy_init", 0xF000, 0x1000),
        ("factory", 0x10000, 0x300000),
        ("blelog", 0x310000, 0xCF0000),
    ];
    let rows: Vec<_> = csv
        .lines()
        .filter(|line| !line.trim_start().starts_with('#') && !line.trim().is_empty())
        .collect();
    assert_eq!(rows.len(), expected.len());
    for (row, (name, offset, size)) in rows.iter().zip(expected) {
        let fields: Vec<_> = row.split(',').map(str::trim).collect();
        assert_eq!(fields[0], name);
        assert_eq!(parse_hex(fields[3]), offset);
        assert_eq!(parse_hex(fields[4]), size);
        assert_eq!(offset & 0xFFF, 0);
        assert_eq!(size & 0xFFF, 0);
    }
    assert_eq!(0x310000 + 0xCF0000, 0x1000000);
}

fn parse_hex(value: &str) -> u32 {
    u32::from_str_radix(value.strip_prefix("0x").unwrap(), 16).unwrap()
}
