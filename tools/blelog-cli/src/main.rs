use std::{
    io::{self, IsTerminal, Write},
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, Context, Result};
use blelog_cli::{
    export::ExportFormat, pull::pull, verify::verify_file, RequestClient, SerialClient,
};
use blelog_format::{
    capture::{CaptureConfig, Profile, RadioStats, Radios, CONFIG_LEN, RADIO_STATS_LEN},
    record::{ScanMode, FORMAT_VERSION},
    transport::message,
    wire::{encode_i64, Info, StartCapture, Stats},
};
use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(version, about = "Control and export ESP32-S3 Wi-Fi / BLE captures")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Ports,
    /// Check the native USB protocol without touching capture or storage.
    Ping {
        #[arg(long)]
        port: String,
    },
    Info {
        #[arg(long)]
        port: String,
    },
    Stats {
        #[arg(long)]
        port: String,
    },
    /// Poll statistics using one persistent serial connection; Ctrl-C exits.
    Watch {
        #[arg(long)]
        port: String,
        #[arg(long, default_value_t = 1.0)]
        interval: f64,
    },
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    Clock {
        #[command(subcommand)]
        command: ClockCommand,
    },
    Capture {
        #[command(subcommand)]
        command: CaptureCommand,
    },
    Pull {
        #[arg(long)]
        port: String,
        #[arg(short, long)]
        output: PathBuf,
        #[arg(long)]
        resume: bool,
        #[arg(long, default_value_t = 4096)]
        chunk_size: u16,
    },
    Verify {
        input: PathBuf,
    },
    Export {
        #[arg(value_enum)]
        format: FormatArg,
        input: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        #[arg(long,value_enum,default_value_t=RadioArg::Both)]
        radio: RadioArg,
    },
    Erase {
        #[arg(long)]
        port: String,
        #[arg(long)]
        yes: bool,
    },
    /// Stop, erase, sync the host clock, then start active capture.
    Prepare {
        #[arg(long)]
        port: String,
        #[arg(long)]
        yes: bool,
        #[command(flatten)]
        options: CaptureOptions,
        /// Also use these capture settings after a power cycle.
        #[arg(long)]
        save: bool,
    },
}

#[derive(Subcommand)]
enum ClockCommand {
    Sync {
        #[arg(long)]
        port: String,
    },
}

#[derive(Subcommand)]
enum CaptureCommand {
    Start {
        #[arg(long)]
        port: String,
        #[command(flatten)]
        options: CaptureOptions,
    },
    Stop {
        #[arg(long)]
        port: String,
    },
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Read settings used by the next powerbank boot.
    Show {
        #[arg(long)]
        port: String,
    },
    /// Stop capture and persist the next-boot profile. Does not erase the log.
    Save {
        #[arg(long)]
        port: String,
        #[command(flatten)]
        options: CaptureOptions,
    },
}
#[derive(Clone, Copy, ValueEnum)]
enum ProfileArg {
    Discovery,
    Raw,
}
#[derive(Clone, Copy, ValueEnum)]
enum RadioArg {
    Ble,
    Wifi,
    Both,
}
#[derive(Clone, Copy, ValueEnum)]
enum TimingArg {
    Balanced,
    BlePriority,
    WifiPriority,
}
#[derive(Args)]
struct CaptureOptions {
    #[arg(long,value_enum,default_value_t=ProfileArg::Raw)]
    profile: ProfileArg,
    #[arg(long,value_enum,default_value_t=RadioArg::Both)]
    radios: RadioArg,
    #[arg(long,value_enum,default_value_t=TimingArg::Balanced)]
    timing: TimingArg,
    #[arg(long,value_enum,default_value_t=ModeArg::Active)]
    mode: ModeArg,
    #[arg(long, default_value_t = 100)]
    interval_ms: u16,
    #[arg(long, default_value_t = 100)]
    window_ms: u16,
    #[arg(long)]
    ble_ms: Option<u16>,
    #[arg(long)]
    wifi_dwell_ms: Option<u16>,
    #[arg(long)]
    channels_per_burst: Option<u8>,
    /// Comma-separated channels; omit to use the country's permitted channels.
    #[arg(long, value_delimiter = ',')]
    channels: Vec<u8>,
    /// ISO country code, or 01 for world-safe channels 1-11.
    #[arg(long, default_value = "01")]
    country: String,
    /// Include failed-FCS driver reports (experimental, off by default).
    #[arg(long)]
    failed_fcs: bool,
}
impl CaptureOptions {
    fn config(&self) -> Result<CaptureConfig> {
        let (ble_ms, burst) = match self.timing {
            TimingArg::Balanced => (1000, 5),
            TimingArg::BlePriority => (1000, 2),
            TimingArg::WifiPriority => (500, 5),
        };
        let country: [u8; 2] = self
            .country
            .to_ascii_uppercase()
            .as_bytes()
            .try_into()
            .context("country must have two characters")?;
        let mut channel_mask = 0;
        for ch in &self.channels {
            if !(1..=13).contains(ch) {
                bail!("channels must be 1-13");
            }
            channel_mask |= 1 << (ch - 1);
        }
        let config = CaptureConfig {
            profile: match self.profile {
                ProfileArg::Discovery => Profile::Discovery,
                ProfileArg::Raw => Profile::Raw,
            },
            radios: match self.radios {
                RadioArg::Ble => Radios::Ble,
                RadioArg::Wifi => Radios::Wifi,
                RadioArg::Both => Radios::Both,
            },
            ble: StartCapture {
                scan_mode: self.mode.into(),
                interval_ms: self.interval_ms,
                window_ms: self.window_ms,
            },
            ble_ms: self.ble_ms.unwrap_or(ble_ms),
            wifi_dwell_ms: self.wifi_dwell_ms.unwrap_or(200),
            channels_per_burst: self.channels_per_burst.unwrap_or(burst),
            channel_mask,
            country,
            failed_fcs: self.failed_fcs,
        };
        config.validate()?;
        Ok(config)
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum ModeArg {
    Active,
    Passive,
}

impl From<ModeArg> for ScanMode {
    fn from(value: ModeArg) -> Self {
        match value {
            ModeArg::Active => Self::Active,
            ModeArg::Passive => Self::Passive,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum FormatArg {
    Jsonl,
    Csv,
    Pcapng,
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Ports => list_ports(),
        Command::Ping { port } => {
            let response = SerialClient::open(&port)?.request(message::PING, &[])?;
            if response != b"BLELOG1" {
                bail!("unexpected PING response: {response:02x?}");
            }
            println!("BLE Logger protocol is responsive");
            Ok(())
        }
        Command::Info { port } => {
            let mut client = SerialClient::open(&port)?;
            print_info(&mut client)
        }
        Command::Stats { port } => {
            let mut client = SerialClient::open(&port)?;
            let stats = Stats::decode(&client.request(message::STATS, &[])?)?;
            println!("received          : {}", stats.received_observations);
            println!("stored            : {}", stats.stored_observations);
            println!("queue drops       : {}", stats.dropped_queue_full);
            println!("storage-full drops: {}", stats.dropped_storage_full);
            println!("flash errors      : {}", stats.flash_write_errors);
            println!("queue high-water  : {}", stats.queue_high_water);
            println!("bytes written     : {}", stats.bytes_written);
            if supports_dual(&mut client)? {
                print_radio_stats(&mut client)?;
            }
            Ok(())
        }
        Command::Watch { port, interval } => {
            if !interval.is_finite() || !(0.1..=60.0).contains(&interval) {
                bail!("interval must be 0.1-60 seconds");
            }
            let mut client = SerialClient::open(&port)?;
            let dual = supports_dual(&mut client)?;
            let mut last = Instant::now();
            let mut last_bytes =
                Stats::decode(&client.request(message::STATS, &[])?)?.bytes_written;
            loop {
                std::thread::sleep(Duration::from_secs_f64(interval));
                let now = Instant::now();
                let stats = Stats::decode(&client.request(message::STATS, &[])?)?;
                let info_bytes = client.request(message::INFO, &[])?;
                let info = Info::decode(&info_bytes)?;
                let rate = stats.bytes_written.saturating_sub(last_bytes) as f64
                    / now.duration_since(last).as_secs_f64();
                println!("{:?} | BLE received={} stored={} queue-drops={} full-drops={} | {:.0} B/s | free={} | remaining={} s",
                    info.state,stats.received_observations,stats.stored_observations,stats.dropped_queue_full,stats.dropped_storage_full,rate,info.free_bytes,
                    if rate>0.0 {format!("{:.0}",info.free_bytes as f64/rate)} else {"unknown".into()});
                if dual {
                    print_radio_stats(&mut client)?;
                }
                last = now;
                last_bytes = stats.bytes_written;
            }
        }
        Command::Config { command } => match command {
            ConfigCommand::Show { port } => {
                let mut client = SerialClient::open(&port)?;
                require_dual(&mut client)?;
                print_config(CaptureConfig::decode(
                    &client.request(message::BOOT_CONFIG, &[])?,
                )?);
                Ok(())
            }
            ConfigCommand::Save { port, options } => {
                let config = options.config()?;
                let mut client = SerialClient::open(&port)?;
                require_dual(&mut client)?;
                client.request(message::STOP_CAPTURE, &[])?;
                send_config(&mut client, message::SAVE_CONFIG, config)?;
                println!("boot profile saved; capture is stopped, existing log preserved");
                print_config(config);
                Ok(())
            }
        },
        Command::Clock {
            command: ClockCommand::Sync { port },
        } => {
            let mut client = SerialClient::open(&port)?;
            sync_clock(&mut client)
        }
        Command::Capture { command } => match command {
            CaptureCommand::Start { port, options } => {
                let config = options.config()?;
                let mut client = SerialClient::open(&port)?;
                start_radios(&mut client, config)
            }
            CaptureCommand::Stop { port } => {
                SerialClient::open(&port)?.request(message::STOP_CAPTURE, &[])?;
                println!("capture stopped");
                Ok(())
            }
        },
        Command::Pull {
            port,
            output,
            resume,
            chunk_size,
        } => {
            let mut client = SerialClient::open(&port)?;
            let summary = pull(&mut client, &output, resume, chunk_size)?;
            println!(
                "saved {} bytes, {} records, {} BLE observations, {} Wi-Fi frames ({} incomplete) to {}",
                summary.bytes,
                summary.records,
                summary.observations,
                summary.wifi_frames,
                summary.incomplete_wifi_frames,
                output.display()
            );
            Ok(())
        }
        Command::Verify { input } => {
            let scan = verify_file(&input)?;
            let wifi = blelog_cli::verify::analyze_wifi(&std::fs::read(&input)?)?;
            println!(
                "valid: {} bytes, {} records, {} BLE observations",
                scan.valid_bytes, scan.record_count, scan.observation_count
            );
            println!(
                "Wi-Fi: {} complete frames ({} metadata-only), {} incomplete; {} unknown records",
                wifi.frames, wifi.metadata_only, wifi.incomplete, wifi.unknown_records
            );
            Ok(())
        }
        Command::Export {
            format,
            input,
            output,
            radio,
        } => {
            let format = match format {
                FormatArg::Jsonl => ExportFormat::Jsonl,
                FormatArg::Csv => ExportFormat::Csv,
                FormatArg::Pcapng => ExportFormat::Pcapng,
            };
            let radios = match radio {
                RadioArg::Ble => Radios::Ble,
                RadioArg::Wifi => Radios::Wifi,
                RadioArg::Both => Radios::Both,
            };
            let count = blelog_cli::export::export_filtered(&input, &output, format, radios)?;
            println!("exported {count} observations to {}", output.display());
            Ok(())
        }
        Command::Erase { port, yes } => {
            confirm_erase(yes)?;
            let mut client = SerialClient::open(&port)?;
            client.request(message::STOP_CAPTURE, &[])?;
            client.request(message::ERASE_LOG, &[])?;
            println!("log erased and verified");
            Ok(())
        }
        Command::Prepare {
            port,
            yes,
            options,
            save,
        } => {
            let config = options.config()?;
            confirm_erase(yes)?;
            let mut client = SerialClient::open(&port)?;
            require_dual(&mut client)?;
            client
                .request(message::STOP_CAPTURE, &[])
                .context("stop failed")?;
            client
                .request(message::ERASE_LOG, &[])
                .context("erase failed")?;
            sync_clock(&mut client)?;
            if save {
                send_config(&mut client, message::SAVE_CONFIG, config)?;
            }
            start_radios(&mut client, config)
        }
    }
}

fn list_ports() -> Result<()> {
    let ports = serialport::available_ports().context("could not enumerate serial ports")?;
    if ports.is_empty() {
        println!("no serial ports found");
    }
    for port in ports {
        println!("{}\t{:?}", port.port_name, port.port_type);
    }
    Ok(())
}

fn print_info(client: &mut impl RequestClient) -> Result<()> {
    let bytes = client.request(message::INFO, &[])?;
    let info = Info::decode(&bytes)?;
    if info.record_format != FORMAT_VERSION {
        bail!("device record format {} is unsupported", info.record_format);
    }
    println!("firmware          : {}", info.firmware_version);
    println!("chip              : {}", info.chip_model);
    println!("base MAC          : {}", format_mac(info.base_mac));
    println!("flash             : {} bytes", info.flash_size);
    println!("PSRAM             : {} bytes", info.psram_size);
    println!("blelog partition  : {} bytes", info.partition_size);
    println!("valid prefix      : {} bytes", info.valid_bytes);
    println!("records           : {}", info.record_count);
    println!("free              : {} bytes", info.free_bytes);
    println!("state             : {:?}", info.state);
    println!(
        "scan              : {:?} {}/{} ms",
        info.scan_mode, info.scan_interval_ms, info.scan_window_ms
    );
    if info.warning_flags != 0 {
        println!("hardware warnings : 0x{:04x}", info.warning_flags);
    }
    if supports_dual(client)? {
        print_config(CaptureConfig::decode(
            &client.request(message::RADIO_INFO, &[])?,
        )?);
    }
    Ok(())
}

fn supports_dual(client: &mut impl RequestClient) -> Result<bool> {
    blelog_cli::client::supports_dual(client)
}
fn require_dual(client: &mut impl RequestClient) -> Result<()> {
    if !supports_dual(client)? {
        bail!("this device needs the Wi-Fi/BLE firmware update");
    }
    Ok(())
}
fn print_config(config: CaptureConfig) {
    println!(
        "capture profile   : {:?}, radios {:?}",
        config.profile, config.radios
    );
    println!(
        "radio timing      : BLE {} ms / Wi-Fi {} x {} ms (+/-20 ms jitter)",
        config.ble_ms, config.channels_per_burst, config.wifi_dwell_ms
    );
    println!(
        "Wi-Fi country     : {}, channels {:?}, failed-FCS {}",
        String::from_utf8_lossy(&config.country),
        config.channels().collect::<Vec<_>>(),
        config.failed_fcs
    );
}
fn print_radio_stats(client: &mut impl RequestClient) -> Result<()> {
    let bytes = client.request(message::RADIO_STATS, &[])?;
    if bytes.len() != RADIO_STATS_LEN + 2 {
        bail!("invalid radio stats length");
    }
    let stats = RadioStats::decode(&bytes[..RADIO_STATS_LEN])?.0;
    println!("radio={} channel={} | Wi-Fi received={} stored={} queue-drops={} full-drops={} error-drops={} invalid={} peak={} | BLE {:.2}s Wi-Fi {:.2}s switch {:.2}s | failures={} max-write={}us",
        bytes[RADIO_STATS_LEN],bytes[RADIO_STATS_LEN+1],stats[0],stats[1],stats[2],stats[3],stats[4],stats[5],stats[6],stats[7] as f64/1e6,stats[8] as f64/1e6,stats[9] as f64/1e6,stats[10],stats[11]);
    Ok(())
}
fn send_config(client: &mut impl RequestClient, command: u8, config: CaptureConfig) -> Result<()> {
    let mut payload = [0; CONFIG_LEN];
    config.encode(&mut payload)?;
    client.request(command, &payload)?;
    Ok(())
}
fn start_radios(client: &mut impl RequestClient, config: CaptureConfig) -> Result<()> {
    if supports_dual(client)? {
        send_config(client, message::START_RADIOS, config)?;
        print_config(config);
        println!("capture started");
        Ok(())
    } else if config.radios == Radios::Ble {
        start_capture(
            client,
            config.ble.scan_mode,
            config.ble.interval_ms,
            config.ble.window_ms,
        )
    } else {
        bail!("this device needs the Wi-Fi/BLE firmware update; --radios ble works with older firmware")
    }
}

fn sync_clock(client: &mut impl RequestClient) -> Result<()> {
    let unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("host clock predates Unix epoch")?
        .as_millis();
    let unix_ms = i64::try_from(unix_ms).context("host timestamp is out of range")?;
    let mut payload = [0u8; 8];
    encode_i64(unix_ms, &mut payload)?;
    client.request(message::SET_CLOCK, &payload)?;
    println!("clock synchronized to {unix_ms} ms");
    Ok(())
}

fn start_capture(
    client: &mut impl RequestClient,
    mode: ScanMode,
    interval_ms: u16,
    window_ms: u16,
) -> Result<()> {
    let settings = StartCapture {
        scan_mode: mode,
        interval_ms,
        window_ms,
    };
    let mut payload = [0u8; StartCapture::LEN];
    settings.encode(&mut payload)?;
    client.request(message::START_CAPTURE, &payload)?;
    println!("capture started: {mode:?}, interval={interval_ms} ms, window={window_ms} ms");
    Ok(())
}

fn confirm_erase(yes: bool) -> Result<()> {
    if yes {
        return Ok(());
    }
    if !io::stdin().is_terminal() {
        bail!("erase requires an interactive terminal or --yes");
    }
    eprint!("Erase every BLE log record on the device? Type 'erase' to continue: ");
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    if answer.trim() != "erase" {
        bail!("erase cancelled");
    }
    Ok(())
}

fn format_mac(bytes: [u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5]
    )
}
