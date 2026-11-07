//! Temporary, standalone board LED test. Never reads, writes, or erases the log.
//! GPIO48: one white flash; GPIO38: two blue flashes. Repeats until reflashed.
use std::{thread::sleep, time::Duration};

use esp_idf_svc::hal::{
    peripherals::Peripherals,
    rmt::{
        config::{MemoryAccess, TxChannelConfig},
        encoder::CopyEncoder,
        PinState, Symbol, TxChannelDriver,
    },
    units::Hertz,
};

const RESOLUTION: Hertz = Hertz(10_000_000);

fn pixel(tx: &mut TxChannelDriver<'_>, grb: [u8; 3]) -> anyhow::Result<()> {
    let zero = Symbol::new_with(
        RESOLUTION,
        PinState::High,
        Duration::from_nanos(300),
        PinState::Low,
        Duration::from_nanos(900),
    )?;
    let one = Symbol::new_with(
        RESOLUTION,
        PinState::High,
        Duration::from_nanos(800),
        PinState::Low,
        Duration::from_nanos(500),
    )?;
    let reset = Symbol::new_half_split(
        RESOLUTION,
        PinState::Low,
        PinState::Low,
        Duration::from_micros(300),
    )?;
    let mut symbols = [reset; 26];
    for (index, byte) in grb.iter().enumerate() {
        for bit in 0..8 {
            symbols[1 + index * 8 + bit] = if byte & (0x80 >> bit) != 0 { one } else { zero };
        }
    }
    tx.send_and_wait(CopyEncoder::new()?, &symbols, &Default::default())?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    let peripherals = Peripherals::take()?;
    let config = TxChannelConfig {
        resolution: RESOLUTION,
        memory_access: MemoryAccess::Indirect {
            memory_block_symbols: 64,
        },
        transaction_queue_depth: 1,
        ..Default::default()
    };
    let mut pin48 = TxChannelDriver::new(peripherals.pins.gpio48, &config)?;
    let mut pin38 = TxChannelDriver::new(peripherals.pins.gpio38, &config)?;
    pixel(&mut pin48, [0; 3])?;
    pixel(&mut pin38, [0; 3])?;
    log::info!(
        "RGB probe: GPIO48 = single white flash, GPIO38 = double blue flash; capture disabled"
    );
    loop {
        pixel(&mut pin48, [24; 3])?;
        sleep(Duration::from_millis(800));
        pixel(&mut pin48, [0; 3])?;
        sleep(Duration::from_millis(1200));
        for _ in 0..2 {
            pixel(&mut pin38, [0, 0, 32])?;
            sleep(Duration::from_millis(300));
            pixel(&mut pin38, [0; 3])?;
            sleep(Duration::from_millis(300));
        }
        sleep(Duration::from_millis(1800));
    }
}
