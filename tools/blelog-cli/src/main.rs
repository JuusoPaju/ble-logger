use std::{
    io::{self, IsTerminal, Write},
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, Context, Result};
use blelog_cli::{
    export::{export_file, ExportFormat},
    pull::pull,
    verify::verify_file,
    RequestClient, SerialClient,
};
use blelog_format::{
    record::{ScanMode, FORMAT_VERSION},
    transport::message,
    wire::{encode_i64, Info, StartCapture, Stats},
};
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(version, about = "Control and export ESP32-S3 BLE Logger captures")]
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
        #[arg(long, default_value_t = 100)]
        interval_ms: u16,
        #[arg(long, default_value_t = 100)]
        window_ms: u16,
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
        #[arg(long, value_enum, default_value_t = ModeArg::Active)]
        mode: ModeArg,
        #[arg(long, default_value_t = 100)]
        interval_ms: u16,
        #[arg(long, default_value_t = 100)]
        window_ms: u16,
    },
    Stop {
        #[arg(long)]
        port: String,
    },
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
            Ok(())
        }
        Command::Clock {
            command: ClockCommand::Sync { port },
        } => {
            let mut client = SerialClient::open(&port)?;
            sync_clock(&mut client)
        }
        Command::Capture { command } => match command {
            CaptureCommand::Start {
                port,
                mode,
                interval_ms,
                window_ms,
            } => {
                let mut client = SerialClient::open(&port)?;
                start_capture(&mut client, mode.into(), interval_ms, window_ms)
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
                "saved {} bytes, {} records, {} observations to {}",
                summary.bytes,
                summary.records,
                summary.observations,
                output.display()
            );
            Ok(())
        }
        Command::Verify { input } => {
            let scan = verify_file(&input)?;
            println!(
                "valid: {} bytes, {} records, {} observations",
                scan.valid_bytes, scan.record_count, scan.observation_count
            );
            Ok(())
        }
        Command::Export {
            format,
            input,
            output,
        } => {
            let format = match format {
                FormatArg::Jsonl => ExportFormat::Jsonl,
                FormatArg::Csv => ExportFormat::Csv,
            };
            let count = export_file(&input, &output, format)?;
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
            interval_ms,
            window_ms,
        } => {
            confirm_erase(yes)?;
            let mut client = SerialClient::open(&port)?;
            client
                .request(message::STOP_CAPTURE, &[])
                .context("stop failed")?;
            client
                .request(message::ERASE_LOG, &[])
                .context("erase failed")?;
            sync_clock(&mut client)?;
            start_capture(&mut client, ScanMode::Active, interval_ms, window_ms)
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
    Ok(())
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
