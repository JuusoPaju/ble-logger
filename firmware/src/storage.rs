use std::{
    ffi::{c_void, CString},
    sync::{
        atomic::{AtomicU8, Ordering},
        mpsc::{self, SyncSender},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use blelog_format::{
    decode_record, encode_record,
    record::{
        encoded_len, BleObservation, ClockSync, Record, RecordError, SessionEnd, SessionStart,
        CRC_LEN, ENVELOPE_LEN, MAGIC, MAX_RECORD_LEN,
    },
    wire::{LoggerState, MAX_DUMP_CHUNK},
};
use crossbeam_channel::{bounded, select, unbounded, Receiver, Sender, TrySendError};
use esp_idf_svc::sys;
use heapless::Vec as HeaplessVec;

use crate::{
    error::{FirmwareError, Result},
    stats::AtomicStats,
};

const RECOVERY_READ_SIZE: usize = 32 * 1024;
const OBSERVATION_QUEUE_CAPACITY: usize = 256;
const DUMP_LEASE: Duration = Duration::from_secs(30);
const PARTITION_LABEL: &str = "blelog";

#[derive(Debug)]
pub struct QueuedObservation {
    pub session_id: u64,
    pub sequence: u32,
    pub monotonic_us: u64,
    pub address: [u8; 6],
    pub address_type: u8,
    pub rssi_dbm: i8,
    pub report_kind: blelog_format::record::ReportKind,
    pub data_status: u8,
    pub primary_phy: u8,
    pub secondary_phy: u8,
    pub advertising_sid: u8,
    pub tx_power_dbm: i8,
    pub legacy_event_type: u8,
    pub extended_properties: u16,
    pub periodic_interval: u16,
    pub payload: HeaplessVec<u8, 255>,
}

impl QueuedObservation {
    fn as_record(&self) -> Record<'_> {
        Record::BleObservation(BleObservation {
            session_id: self.session_id,
            sequence: self.sequence,
            monotonic_us: self.monotonic_us,
            address: self.address,
            address_type: self.address_type,
            rssi_dbm: self.rssi_dbm,
            report_kind: self.report_kind,
            data_status: self.data_status,
            primary_phy: self.primary_phy,
            secondary_phy: self.secondary_phy,
            advertising_sid: self.advertising_sid,
            tx_power_dbm: self.tx_power_dbm,
            legacy_event_type: self.legacy_event_type,
            extended_properties: self.extended_properties,
            periodic_interval: self.periodic_interval,
            payload: &self.payload,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct StorageSnapshot {
    pub partition_size: u32,
    pub valid_bytes: u32,
    pub record_count: u64,
    pub state: LoggerState,
}

impl StorageSnapshot {
    pub fn free_bytes(self) -> u32 {
        self.partition_size.saturating_sub(self.valid_bytes)
    }
}

#[derive(Clone, Copy)]
struct SessionBaseline {
    session_id: u64,
    received: u64,
    stored: u64,
    queue_drops: u64,
    full_drops: u64,
    flash_errors: u64,
}

enum Command {
    Snapshot(SyncSender<Result<StorageSnapshot>>),
    BeginSession(SessionStart<'static>, SyncSender<Result<()>>),
    FinishSession {
        session_id: u64,
        stop_monotonic_us: u64,
        reply: SyncSender<Result<()>>,
    },
    ClockSync(ClockSync, SyncSender<Result<()>>),
    DumpBegin(SyncSender<Result<(u32, u32, u64)>>),
    DumpRead {
        transfer_id: u32,
        offset: u32,
        length: u16,
        reply: SyncSender<Result<Vec<u8>>>,
    },
    DumpEnd(u32, SyncSender<Result<()>>),
    Erase(SyncSender<Result<()>>),
}

#[derive(Clone)]
pub struct StorageService {
    observations: Sender<QueuedObservation>,
    commands: Sender<Command>,
    pub stats: Arc<AtomicStats>,
    state: Arc<AtomicU8>,
}

impl StorageService {
    pub fn start(stats: Arc<AtomicStats>, state: Arc<AtomicU8>) -> Result<Self> {
        let (observations, observation_rx) = bounded(OBSERVATION_QUEUE_CAPACITY);
        let (commands, command_rx) = unbounded();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let worker_stats = Arc::clone(&stats);
        let worker_state = Arc::clone(&state);
        thread::Builder::new()
            .name("blelog-storage".into())
            .stack_size(64 * 1024)
            .spawn(move || {
                worker_state.store(LoggerState::Recovering as u8, Ordering::Release);
                match FlashLog::open_and_recover() {
                    Ok(log) => {
                        let recovered_state = log.recovered_state;
                        worker_state.store(recovered_state as u8, Ordering::Release);
                        let _ = ready_tx.send(Ok(log.snapshot(recovered_state)));
                        storage_loop(log, observation_rx, command_rx, worker_stats, worker_state);
                    }
                    Err(error) => {
                        worker_state.store(LoggerState::Error as u8, Ordering::Release);
                        let _ = ready_tx.send(Err(error));
                    }
                }
            })
            .map_err(|_| FirmwareError::QueueClosed)?;
        ready_rx.recv().map_err(|_| FirmwareError::QueueClosed)??;
        Ok(Self {
            observations,
            commands,
            stats,
            state,
        })
    }

    pub fn state(&self) -> LoggerState {
        LoggerState::try_from(self.state.load(Ordering::Acquire)).unwrap_or(LoggerState::Error)
    }

    pub fn set_state(&self, state: LoggerState) {
        self.state.store(state as u8, Ordering::Release);
    }

    pub fn submit(&self, observation: QueuedObservation) {
        match self.state() {
            LoggerState::Full => {
                self.stats.full_drops.fetch_add(1, Ordering::Relaxed);
                return;
            }
            LoggerState::Error => {
                self.stats.queue_drops.fetch_add(1, Ordering::Relaxed);
                return;
            }
            _ => {}
        }
        match self.observations.try_send(observation) {
            Ok(()) => self.stats.observe_queue_depth(self.observations.len()),
            Err(TrySendError::Full(_)) => {
                self.stats.queue_drops.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => {
                self.stats.flash_errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub fn snapshot(&self) -> Result<StorageSnapshot> {
        self.call(Command::Snapshot)
    }

    pub fn begin_session(&self, start: SessionStart<'static>) -> Result<()> {
        self.call(|reply| Command::BeginSession(start, reply))
    }

    pub fn finish_session(&self, session_id: u64, stop_monotonic_us: u64) -> Result<()> {
        self.call(|reply| Command::FinishSession {
            session_id,
            stop_monotonic_us,
            reply,
        })
    }

    pub fn clock_sync(&self, sync: ClockSync) -> Result<()> {
        self.call(|reply| Command::ClockSync(sync, reply))
    }

    pub fn dump_begin(&self) -> Result<(u32, u32, u64)> {
        self.call(Command::DumpBegin)
    }

    pub fn dump_read(&self, transfer_id: u32, offset: u32, length: u16) -> Result<Vec<u8>> {
        self.call(|reply| Command::DumpRead {
            transfer_id,
            offset,
            length,
            reply,
        })
    }

    pub fn dump_end(&self, transfer_id: u32) -> Result<()> {
        self.call(|reply| Command::DumpEnd(transfer_id, reply))
    }

    pub fn erase(&self) -> Result<()> {
        self.call(Command::Erase)
    }

    fn call<T>(&self, make: impl FnOnce(SyncSender<Result<T>>) -> Command) -> Result<T> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.commands
            .send(make(tx))
            .map_err(|_| FirmwareError::QueueClosed)?;
        rx.recv().map_err(|_| FirmwareError::QueueClosed)?
    }
}

struct DumpLeaseState {
    transfer_id: u32,
    frozen_bytes: u32,
    previous_state: LoggerState,
    last_activity: Instant,
}

fn storage_loop(
    mut log: FlashLog,
    observations: Receiver<QueuedObservation>,
    commands: Receiver<Command>,
    stats: Arc<AtomicStats>,
    state: Arc<AtomicU8>,
) {
    let mut session: Option<SessionBaseline> = None;
    let mut lease: Option<DumpLeaseState> = None;
    loop {
        select! {
            recv(commands) -> command => match command {
                Ok(command) => handle_command(command, &mut log, &observations, &stats, &state, &mut session, &mut lease),
                Err(_) => break,
            },
            recv(observations) -> observation => match observation {
                Ok(observation) => store_observation(&mut log, observation, &stats, &state),
                Err(_) => break,
            },
            default(Duration::from_millis(250)) => {}
        }
        if lease
            .as_ref()
            .is_some_and(|lease| lease.last_activity.elapsed() >= DUMP_LEASE)
        {
            let previous = lease.take().unwrap().previous_state;
            state.store(previous as u8, Ordering::Release);
        }
    }
    state.store(LoggerState::Error as u8, Ordering::Release);
}

fn handle_command(
    command: Command,
    log: &mut FlashLog,
    observations: &Receiver<QueuedObservation>,
    stats: &AtomicStats,
    state: &AtomicU8,
    session: &mut Option<SessionBaseline>,
    lease: &mut Option<DumpLeaseState>,
) {
    match command {
        Command::Snapshot(reply) => {
            let _ = reply.send(Ok(log.snapshot(current_state(state))));
        }
        Command::BeginSession(start, reply) => {
            let result = if current_state(state) != LoggerState::Ready {
                Err(FirmwareError::InvalidState)
            } else {
                append_with_error_accounting(log, &Record::SessionStart(start), true, stats, state)
                    .map(|_| {
                        let now = stats.snapshot();
                        *session = Some(SessionBaseline {
                            session_id: start.session_id,
                            received: now.received_observations,
                            stored: now.stored_observations,
                            queue_drops: now.dropped_queue_full,
                            full_drops: now.dropped_storage_full,
                            flash_errors: now.flash_write_errors,
                        });
                        state.store(LoggerState::Capturing as u8, Ordering::Release);
                    })
            };
            let _ = reply.send(result);
        }
        Command::FinishSession {
            session_id,
            stop_monotonic_us,
            reply,
        } => {
            while let Ok(observation) = observations.try_recv() {
                store_observation(log, observation, stats, state);
            }
            let result = if let Some(baseline) = session.take() {
                if baseline.session_id != session_id {
                    Err(FirmwareError::InvalidState)
                } else {
                    let now = stats.snapshot();
                    let end = SessionEnd {
                        session_id,
                        stop_monotonic_us,
                        total_observations: now
                            .received_observations
                            .saturating_sub(baseline.received),
                        stored_observations: now
                            .stored_observations
                            .saturating_sub(baseline.stored),
                        dropped_queue_full: now
                            .dropped_queue_full
                            .saturating_sub(baseline.queue_drops),
                        dropped_storage_full: now
                            .dropped_storage_full
                            .saturating_sub(baseline.full_drops),
                        flash_write_errors: now
                            .flash_write_errors
                            .saturating_sub(baseline.flash_errors),
                    };
                    append_with_error_accounting(log, &Record::SessionEnd(end), false, stats, state)
                        .map(|_| ())
                }
            } else {
                Ok(()) // STOP_CAPTURE is idempotent while already stopped.
            };
            if current_state(state) != LoggerState::Error {
                let next = if log.full {
                    LoggerState::Full
                } else {
                    log.recovered_state
                };
                state.store(next as u8, Ordering::Release);
            }
            let _ = reply.send(result);
        }
        Command::ClockSync(sync, reply) => {
            let result = match current_state(state) {
                LoggerState::Capturing => {
                    append_with_error_accounting(log, &Record::ClockSync(sync), true, stats, state)
                        .map(|_| ())
                }
                LoggerState::Ready => Ok(()),
                _ => Err(FirmwareError::InvalidState),
            };
            let _ = reply.send(result);
        }
        Command::DumpBegin(reply) => {
            let current = current_state(state);
            let result = if lease.is_some() {
                Err(FirmwareError::Busy)
            } else if matches!(
                current,
                LoggerState::Ready | LoggerState::Full | LoggerState::CorruptTail
            ) {
                let transfer_id = unsafe { sys::esp_random() };
                *lease = Some(DumpLeaseState {
                    transfer_id,
                    frozen_bytes: log.append_offset as u32,
                    previous_state: current,
                    last_activity: Instant::now(),
                });
                state.store(LoggerState::Dumping as u8, Ordering::Release);
                Ok((transfer_id, log.append_offset as u32, log.record_count))
            } else {
                Err(FirmwareError::InvalidState)
            };
            let _ = reply.send(result);
        }
        Command::DumpRead {
            transfer_id,
            offset,
            length,
            reply,
        } => {
            let result = if length == 0 || length > MAX_DUMP_CHUNK {
                Err(FirmwareError::Bounds)
            } else if let Some(active) = lease.as_mut() {
                if active.transfer_id != transfer_id
                    || offset > active.frozen_bytes
                    || u32::from(length) > active.frozen_bytes - offset
                {
                    Err(FirmwareError::Bounds)
                } else {
                    active.last_activity = Instant::now();
                    let mut bytes = vec![0u8; usize::from(length)];
                    log.read(offset as usize, &mut bytes).map(|_| bytes)
                }
            } else {
                Err(FirmwareError::InvalidState)
            };
            let _ = reply.send(result);
        }
        Command::DumpEnd(transfer_id, reply) => {
            let result = if lease
                .as_ref()
                .is_some_and(|active| active.transfer_id == transfer_id)
            {
                let previous = lease.take().unwrap().previous_state;
                state.store(previous as u8, Ordering::Release);
                Ok(())
            } else {
                Err(FirmwareError::InvalidState)
            };
            let _ = reply.send(result);
        }
        Command::Erase(reply) => {
            let current = current_state(state);
            if lease.is_some()
                || !matches!(
                    current,
                    LoggerState::Ready | LoggerState::Full | LoggerState::CorruptTail
                )
            {
                let _ = reply.send(Err(FirmwareError::InvalidState));
            } else {
                state.store(LoggerState::Erasing as u8, Ordering::Release);
                let result = match log.erase_and_verify() {
                    Ok(()) => {
                        *session = None;
                        state.store(LoggerState::Ready as u8, Ordering::Release);
                        Ok(())
                    }
                    Err(error) => {
                        state.store(LoggerState::Error as u8, Ordering::Release);
                        Err(error)
                    }
                };
                let _ = reply.send(result);
            }
        }
    }
}

fn store_observation(
    log: &mut FlashLog,
    observation: QueuedObservation,
    stats: &AtomicStats,
    state: &AtomicU8,
) {
    if current_state(state) == LoggerState::Error {
        stats.queue_drops.fetch_add(1, Ordering::Relaxed);
        return;
    }
    if current_state(state) == LoggerState::Full || log.full {
        stats.full_drops.fetch_add(1, Ordering::Relaxed);
        return;
    }
    match append_with_error_accounting(log, &observation.as_record(), true, stats, state) {
        Ok(bytes) => {
            stats.stored.fetch_add(1, Ordering::Relaxed);
            stats
                .bytes_written
                .fetch_add(bytes as u32, Ordering::Relaxed);
        }
        Err(FirmwareError::Full) => {
            stats.full_drops.fetch_add(1, Ordering::Relaxed);
            log.full = true;
            state.store(LoggerState::Full as u8, Ordering::Release);
            crate::ble::request_stop();
        }
        Err(_) => {
            state.store(LoggerState::Error as u8, Ordering::Release);
            crate::ble::request_stop();
        }
    }
}

fn append_with_error_accounting(
    log: &mut FlashLog,
    record: &Record<'_>,
    reserve_end: bool,
    stats: &AtomicStats,
    state: &AtomicU8,
) -> Result<usize> {
    match log.append(record, reserve_end) {
        Ok(len) => Ok(len),
        Err(error @ (FirmwareError::FlashWrite(_) | FirmwareError::Record(_))) => {
            stats.flash_errors.fetch_add(1, Ordering::Relaxed);
            state.store(LoggerState::Error as u8, Ordering::Release);
            Err(error)
        }
        Err(error @ FirmwareError::Full) => {
            log.full = true;
            state.store(LoggerState::Full as u8, Ordering::Release);
            Err(error)
        }
        Err(error) => Err(error),
    }
}

fn current_state(state: &AtomicU8) -> LoggerState {
    LoggerState::try_from(state.load(Ordering::Acquire)).unwrap_or(LoggerState::Error)
}

struct FlashLog {
    partition: *const sys::esp_partition_t,
    partition_size: usize,
    erase_size: usize,
    append_offset: usize,
    record_count: u64,
    recovered_state: LoggerState,
    full: bool,
}

// The pointer refers to ESP-IDF's static partition table for the process lifetime.
unsafe impl Send for FlashLog {}

impl FlashLog {
    fn open_and_recover() -> Result<Self> {
        let label = CString::new(PARTITION_LABEL).unwrap();
        let partition = unsafe {
            sys::esp_partition_find_first(
                sys::esp_partition_type_t_ESP_PARTITION_TYPE_DATA,
                0x40,
                label.as_ptr(),
            )
        };
        if partition.is_null() {
            return Err(FirmwareError::PartitionNotFound);
        }
        let partition_size = unsafe { (*partition).size as usize };
        let erase_size = unsafe { (*partition).erase_size as usize }.max(4096);
        let mut log = Self {
            partition,
            partition_size,
            erase_size,
            append_offset: 0,
            record_count: 0,
            recovered_state: LoggerState::Ready,
            full: false,
        };
        log.recover()?;
        Ok(log)
    }

    fn snapshot(&self, state: LoggerState) -> StorageSnapshot {
        StorageSnapshot {
            partition_size: self.partition_size as u32,
            valid_bytes: self.append_offset as u32,
            record_count: self.record_count,
            state,
        }
    }

    fn recover(&mut self) -> Result<()> {
        let mut chunk = [0u8; RECOVERY_READ_SIZE];
        let mut record = [0u8; MAX_RECORD_LEN];
        let mut offset = 0usize;
        while offset < self.partition_size {
            let read_len = RECOVERY_READ_SIZE.min(self.partition_size - offset);
            self.read(offset, &mut chunk[..read_len])?;
            let mut local = 0usize;
            while local < read_len {
                let absolute = offset + local;
                if chunk[local] == 0xFF {
                    if self.is_erased_from(absolute, &mut chunk)? {
                        self.append_offset = absolute;
                        self.recovered_state = LoggerState::Ready;
                        return Ok(());
                    }
                    self.append_offset = absolute;
                    self.recovered_state = LoggerState::CorruptTail;
                    return Ok(());
                }

                let available = &chunk[local..read_len];
                let decoded = if available.len() >= MAX_RECORD_LEN {
                    decode_record(available)
                } else {
                    let record_len = MAX_RECORD_LEN.min(self.partition_size - absolute);
                    self.read(absolute, &mut record[..record_len])?;
                    decode_record(&record[..record_len])
                };
                match decoded {
                    Ok(decoded) => {
                        self.record_count += 1;
                        local += decoded.total_len;
                    }
                    Err(RecordError::UnsupportedVersion(version)) => {
                        self.append_offset = absolute;
                        self.recovered_state = LoggerState::CorruptTail;
                        log::error!(
                            "unsupported record format {version} at offset 0x{absolute:08x}"
                        );
                        return Ok(());
                    }
                    Err(_) => {
                        self.append_offset = absolute;
                        self.recovered_state = LoggerState::CorruptTail;
                        return Ok(());
                    }
                }
            }
            offset += local;
        }
        self.append_offset = offset;
        self.recovered_state = LoggerState::Full;
        self.full = true;
        Ok(())
    }

    fn is_erased_from(
        &self,
        mut offset: usize,
        scratch: &mut [u8; RECOVERY_READ_SIZE],
    ) -> Result<bool> {
        while offset < self.partition_size {
            let len = scratch.len().min(self.partition_size - offset);
            self.read(offset, &mut scratch[..len])?;
            if scratch[..len].iter().any(|&byte| byte != 0xFF) {
                return Ok(false);
            }
            offset += len;
        }
        Ok(true)
    }

    fn append(&mut self, record: &Record<'_>, reserve_end: bool) -> Result<usize> {
        if self.recovered_state == LoggerState::CorruptTail {
            return Err(FirmwareError::CorruptTail);
        }
        let len = encoded_len(record)?;
        let end_reserve = if reserve_end { SESSION_END_MAX_LEN } else { 0 };
        if self.append_offset + len + end_reserve > self.partition_size {
            self.full = true;
            return Err(FirmwareError::Full);
        }
        let mut scratch = [0u8; MAX_RECORD_LEN];
        let encoded = encode_record(record, &mut scratch)?;
        debug_assert_eq!(encoded, len);
        // Phase one: body through CRC. Phase two: magic commit marker.
        self.write(self.append_offset + MAGIC.len(), &scratch[MAGIC.len()..len])?;
        self.write(self.append_offset, &scratch[..MAGIC.len()])?;
        self.append_offset += len;
        self.record_count += 1;
        Ok(len)
    }

    fn read(&self, offset: usize, output: &mut [u8]) -> Result<()> {
        if offset
            .checked_add(output.len())
            .is_none_or(|end| end > self.partition_size)
        {
            return Err(FirmwareError::Bounds);
        }
        let rc = unsafe {
            sys::esp_partition_read(
                self.partition,
                offset,
                output.as_mut_ptr().cast::<c_void>(),
                output.len(),
            )
        };
        if rc == sys::ESP_OK {
            Ok(())
        } else {
            Err(FirmwareError::FlashRead(rc))
        }
    }

    fn write(&self, offset: usize, input: &[u8]) -> Result<()> {
        if offset
            .checked_add(input.len())
            .is_none_or(|end| end > self.partition_size)
        {
            return Err(FirmwareError::Bounds);
        }
        let rc = unsafe {
            sys::esp_partition_write(
                self.partition,
                offset,
                input.as_ptr().cast::<c_void>(),
                input.len(),
            )
        };
        if rc == sys::ESP_OK {
            Ok(())
        } else {
            Err(FirmwareError::FlashWrite(rc))
        }
    }

    fn erase_and_verify(&mut self) -> Result<()> {
        if self.partition_size % self.erase_size != 0 {
            return Err(FirmwareError::Bounds);
        }
        let rc = unsafe { sys::esp_partition_erase_range(self.partition, 0, self.partition_size) };
        if rc != sys::ESP_OK {
            return Err(FirmwareError::FlashErase(rc));
        }
        let mut sector = vec![0u8; self.erase_size];
        self.read(0, &mut sector)?;
        if sector.iter().any(|&byte| byte != 0xFF) {
            return Err(FirmwareError::FlashErase(-1));
        }
        self.read(self.partition_size - self.erase_size, &mut sector)?;
        if sector.iter().any(|&byte| byte != 0xFF) {
            return Err(FirmwareError::FlashErase(-1));
        }
        self.append_offset = 0;
        self.record_count = 0;
        self.recovered_state = LoggerState::Ready;
        self.full = false;
        Ok(())
    }
}

const SESSION_END_MAX_LEN: usize = 72;

const _: () = {
    assert!(ENVELOPE_LEN + CRC_LEN <= MAX_RECORD_LEN);
    assert!(SESSION_END_MAX_LEN <= MAX_RECORD_LEN);
};
