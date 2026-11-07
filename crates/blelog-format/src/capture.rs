//! Versioned dual-radio settings and additive v1 record bodies.
use crate::{
    record::RecordError,
    wire::{StartCapture, WireError},
    ScanMode,
};

pub const CONFIG_LEN: usize = 20;
pub const WIFI_MAX_LEN: usize = 4095;
pub const FRAGMENT_LEN: usize = 896;
pub const TYPE_CONFIG: u8 = 5;
pub const TYPE_WIFI: u8 = 6;
pub const TYPE_WINDOW: u8 = 7;
pub const TYPE_RADIO_STATS: u8 = 8;
pub const WIFI_METADATA_ONLY: u8 = 1;
pub const WIFI_FCS_INCLUDED: u8 = 2;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Discovery = 0,
    Raw = 1,
}
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Radios {
    Ble = 1,
    Wifi = 2,
    Both = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureConfig {
    pub profile: Profile,
    pub radios: Radios,
    pub ble: StartCapture,
    pub ble_ms: u16,
    pub wifi_dwell_ms: u16,
    pub channels_per_burst: u8,
    /// Bit zero denotes channel 1. Zero asks the driver for all permitted channels.
    pub channel_mask: u16,
    /// ISO country or IDF world-safe "01".
    pub country: [u8; 2],
    pub failed_fcs: bool,
}
impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            profile: Profile::Raw,
            radios: Radios::Both,
            ble: StartCapture {
                scan_mode: ScanMode::Active,
                interval_ms: 100,
                window_ms: 100,
            },
            ble_ms: 1000,
            wifi_dwell_ms: 200,
            channels_per_burst: 5,
            channel_mask: 0,
            country: *b"01",
            failed_fcs: false,
        }
    }
}
impl CaptureConfig {
    pub fn validate(&self) -> Result<(), WireError> {
        self.ble.encode(&mut [0; StartCapture::LEN])?;
        if !(100..=2000).contains(&self.ble_ms)
            || self.ble_ms % 10 != 0
            || !(50..=1000).contains(&self.wifi_dwell_ms)
            || !(1..=13).contains(&self.channels_per_burst)
            || self.channel_mask & !0x1fff != 0
            || !(self.country == *b"01" || self.country.iter().all(u8::is_ascii_uppercase))
        {
            return Err(WireError::InvalidField("capture_config"));
        }
        Ok(())
    }
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, WireError> {
        self.validate()?;
        if out.len() < CONFIG_LEN {
            return Err(WireError::TooLong);
        }
        out[..CONFIG_LEN].fill(0);
        out[0] = 1;
        out[1] = self.profile as u8;
        out[2] = self.radios as u8;
        self.ble.encode(&mut out[3..8])?;
        out[8..10].copy_from_slice(&self.ble_ms.to_le_bytes());
        out[10..12].copy_from_slice(&self.wifi_dwell_ms.to_le_bytes());
        out[12] = self.channels_per_burst;
        out[13..15].copy_from_slice(&self.channel_mask.to_le_bytes());
        out[15..17].copy_from_slice(&self.country);
        out[17] = self.failed_fcs as u8;
        Ok(CONFIG_LEN)
    }
    pub fn decode(b: &[u8]) -> Result<Self, WireError> {
        if b.len() != CONFIG_LEN || b[0] != 1 || b[17] > 1 || b[18..20] != [0, 0] {
            return Err(WireError::InvalidField("capture_config_version/length"));
        }
        let value = Self {
            profile: match b[1] {
                0 => Profile::Discovery,
                1 => Profile::Raw,
                _ => return Err(WireError::InvalidField("profile")),
            },
            radios: match b[2] {
                1 => Radios::Ble,
                2 => Radios::Wifi,
                3 => Radios::Both,
                _ => return Err(WireError::InvalidField("radios")),
            },
            ble: StartCapture::decode(&b[3..8])?,
            ble_ms: u16::from_le_bytes(b[8..10].try_into().unwrap()),
            wifi_dwell_ms: u16::from_le_bytes(b[10..12].try_into().unwrap()),
            channels_per_burst: b[12],
            channel_mask: u16::from_le_bytes(b[13..15].try_into().unwrap()),
            country: b[15..17].try_into().unwrap(),
            failed_fcs: b[17] != 0,
        };
        value.validate()?;
        Ok(value)
    }
    pub fn channels(&self) -> impl Iterator<Item = u8> {
        let mask = self.channel_mask;
        (1..=13).filter(move |ch| mask & (1 << (ch - 1)) != 0)
    }
}

/// Cyclic channel order persists across bursts. No channel is starved.
pub struct ChannelCursor {
    mask: u16,
    next: u8,
}
impl ChannelCursor {
    pub fn new(mask: u16, seed: u32) -> Result<Self, WireError> {
        if mask == 0 || mask & !0x1fff != 0 {
            return Err(WireError::InvalidField("channels"));
        }
        Ok(Self {
            mask,
            next: (seed % 13) as u8 + 1,
        })
    }
    pub fn next_channel(&mut self) -> u8 {
        loop {
            let ch = self.next;
            self.next = ch % 13 + 1;
            if self.mask & (1 << (ch - 1)) != 0 {
                return ch;
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WifiMeta {
    pub session_id: u64,
    pub sequence: u32,
    pub monotonic_us: u64,
    pub window_id: u32,
    pub hardware_timestamp_us: u32,
    pub original_len: u16,
    pub captured_len: u16,
    pub channel: u8,
    pub secondary_channel: u8,
    pub rssi_dbm: i8,
    pub packet_type: u8,
    pub rate: u8,
    pub sig_mode: u8,
    pub mcs: u8,
    pub bandwidth: u8,
    pub aggregation: u8,
    pub rx_state: u8,
    pub flags: u8,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WifiFragment<'a> {
    pub meta: WifiMeta,
    pub offset: u16,
    pub bytes: &'a [u8],
}
impl WifiFragment<'_> {
    pub fn validate(&self) -> Result<(), RecordError> {
        let m = self.meta;
        if m.original_len as usize > WIFI_MAX_LEN
            || m.captured_len > m.original_len
            || self.bytes.len() > FRAGMENT_LEN
            || self.offset as usize + self.bytes.len() > m.captured_len as usize
            || self.offset as usize % FRAGMENT_LEN != 0
            || (self.offset as usize + self.bytes.len() < m.captured_len as usize
                && self.bytes.len() != FRAGMENT_LEN)
            || (m.captured_len != 0 && self.bytes.is_empty())
            || m.packet_type > 3
            || m.channel == 0
            || m.channel > 13
            || m.flags & !(WIFI_METADATA_ONLY | WIFI_FCS_INCLUDED) != 0
            || (m.flags & WIFI_METADATA_ONLY != 0 && m.captured_len != 0)
        {
            return Err(RecordError::InvalidField("wifi_fragment"));
        }
        Ok(())
    }
}

pub const RADIO_STATS_FIELDS: usize = 12;
pub const RADIO_STATS_LEN: usize = RADIO_STATS_FIELDS * 8;
/// Wi-Fi received, stored, queue drops, full drops, error drops, invalid reports,
/// Wi-Fi queue peak, BLE window us, Wi-Fi window us, switch us, switch failures,
/// maximum flash commit us. Counters are session-scoped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RadioStats(pub [u64; RADIO_STATS_FIELDS]);
impl RadioStats {
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, WireError> {
        if out.len() < RADIO_STATS_LEN {
            return Err(WireError::TooLong);
        }
        for (v, b) in self.0.iter().zip(out.chunks_exact_mut(8)) {
            b.copy_from_slice(&v.to_le_bytes());
        }
        Ok(RADIO_STATS_LEN)
    }
    pub fn decode(b: &[u8]) -> Result<Self, WireError> {
        if b.len() != RADIO_STATS_LEN {
            return Err(WireError::InvalidField("radio_stats"));
        }
        let mut result = Self::default();
        for (v, b) in result.0.iter_mut().zip(b.chunks_exact(8)) {
            *v = u64::from_le_bytes(b.try_into().unwrap());
        }
        Ok(result)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureRecord<'a> {
    Config {
        session_id: u64,
        seed: u32,
        config: CaptureConfig,
    },
    Wifi(WifiFragment<'a>),
    Window {
        session_id: u64,
        window_id: u32,
        monotonic_us: u64,
        radio: u8,
        channel: u8,
        begin: bool,
    },
    Stats {
        session_id: u64,
        stats: RadioStats,
    },
}
impl CaptureRecord<'_> {
    pub fn record_type(&self) -> u8 {
        match self {
            Self::Config { .. } => TYPE_CONFIG,
            Self::Wifi(_) => TYPE_WIFI,
            Self::Window { .. } => TYPE_WINDOW,
            Self::Stats { .. } => TYPE_RADIO_STATS,
        }
    }
    pub fn body_len(&self) -> Result<usize, RecordError> {
        Ok(match self {
            Self::Config { config, .. } => {
                config
                    .validate()
                    .map_err(|_| RecordError::InvalidField("config"))?;
                12 + CONFIG_LEN
            }
            Self::Wifi(f) => {
                f.validate()?;
                45 + f.bytes.len()
            }
            Self::Window { radio, channel, .. } => {
                if !matches!(radio, 1 | 2)
                    || (*radio == 1 && *channel != 0)
                    || (*radio == 2 && !(1..=13).contains(channel))
                {
                    return Err(RecordError::InvalidField("radio_window"));
                }
                23
            }
            Self::Stats { .. } => 8 + RADIO_STATS_LEN,
        })
    }
    pub fn encode_body(&self, out: &mut [u8]) -> Result<(), RecordError> {
        let n = self.body_len()?;
        if out.len() != n {
            return Err(RecordError::BufferTooSmall);
        }
        let mut w = Writer { b: out, pos: 0 };
        match self {
            Self::Config {
                session_id,
                seed,
                config,
            } => {
                w.put(&session_id.to_le_bytes());
                w.put(&seed.to_le_bytes());
                config
                    .encode(&mut w.b[w.pos..])
                    .map_err(|_| RecordError::InvalidField("config"))?;
            }
            Self::Wifi(f) => {
                let m = f.meta;
                w.put(&m.session_id.to_le_bytes());
                w.put(&m.sequence.to_le_bytes());
                w.put(&m.monotonic_us.to_le_bytes());
                w.put(&m.window_id.to_le_bytes());
                w.put(&m.hardware_timestamp_us.to_le_bytes());
                w.put(&m.original_len.to_le_bytes());
                w.put(&m.captured_len.to_le_bytes());
                w.put(&f.offset.to_le_bytes());
                w.put(&[
                    m.channel,
                    m.secondary_channel,
                    m.rssi_dbm as u8,
                    m.packet_type,
                    m.rate,
                    m.sig_mode,
                    m.mcs,
                    m.bandwidth,
                    m.aggregation,
                    m.rx_state,
                    m.flags,
                ]);
                w.put(f.bytes);
            }
            Self::Window {
                session_id,
                window_id,
                monotonic_us,
                radio,
                channel,
                begin,
            } => {
                w.put(&session_id.to_le_bytes());
                w.put(&window_id.to_le_bytes());
                w.put(&monotonic_us.to_le_bytes());
                w.put(&[*radio, *channel, *begin as u8]);
            }
            Self::Stats { session_id, stats } => {
                w.put(&session_id.to_le_bytes());
                stats
                    .encode(&mut w.b[w.pos..])
                    .map_err(|_| RecordError::InvalidField("stats"))?;
            }
        }
        Ok(())
    }
}
struct Writer<'a> {
    b: &'a mut [u8],
    pos: usize,
}
impl Writer<'_> {
    fn put(&mut self, b: &[u8]) {
        self.b[self.pos..self.pos + b.len()].copy_from_slice(b);
        self.pos += b.len();
    }
}
fn u16at(b: &[u8], n: usize) -> u16 {
    u16::from_le_bytes(b[n..n + 2].try_into().unwrap())
}
fn u32at(b: &[u8], n: usize) -> u32 {
    u32::from_le_bytes(b[n..n + 4].try_into().unwrap())
}
fn u64at(b: &[u8], n: usize) -> u64 {
    u64::from_le_bytes(b[n..n + 8].try_into().unwrap())
}
pub fn decode_body(kind: u8, b: &[u8]) -> Result<CaptureRecord<'_>, RecordError> {
    let bad = || RecordError::InvalidField("capture_body");
    let result = match kind {
        TYPE_CONFIG if b.len() == 12 + CONFIG_LEN => CaptureRecord::Config {
            session_id: u64at(b, 0),
            seed: u32at(b, 8),
            config: CaptureConfig::decode(&b[12..]).map_err(|_| bad())?,
        },
        TYPE_WIFI if b.len() >= 45 => CaptureRecord::Wifi(WifiFragment {
            meta: WifiMeta {
                session_id: u64at(b, 0),
                sequence: u32at(b, 8),
                monotonic_us: u64at(b, 12),
                window_id: u32at(b, 20),
                hardware_timestamp_us: u32at(b, 24),
                original_len: u16at(b, 28),
                captured_len: u16at(b, 30),
                channel: b[34],
                secondary_channel: b[35],
                rssi_dbm: b[36] as i8,
                packet_type: b[37],
                rate: b[38],
                sig_mode: b[39],
                mcs: b[40],
                bandwidth: b[41],
                aggregation: b[42],
                rx_state: b[43],
                flags: b[44],
            },
            offset: u16at(b, 32),
            bytes: &b[45..],
        }),
        TYPE_WINDOW if b.len() == 23 && b[22] <= 1 => CaptureRecord::Window {
            session_id: u64at(b, 0),
            window_id: u32at(b, 8),
            monotonic_us: u64at(b, 12),
            radio: b[20],
            channel: b[21],
            begin: b[22] != 0,
        },
        TYPE_RADIO_STATS if b.len() == 8 + RADIO_STATS_LEN => CaptureRecord::Stats {
            session_id: u64at(b, 0),
            stats: RadioStats::decode(&b[8..]).map_err(|_| bad())?,
        },
        _ => return Err(bad()),
    };
    result.body_len()?;
    Ok(result)
}
