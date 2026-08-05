// SPDX-License-Identifier: LGPL-3.0-or-later

//! Single-owner controller worker.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};

use crate::device::{self, Device};
use crate::flux::{RevolutionScan, scan_revolution};
use crate::{
    BridgeConfig, BridgeEvent, CaptureQuality, DriveStatus, Error, PortId, ReadMode, Result,
    TrackAddress, TrackCapture, WriteId, WriteRequest,
};

const COMMAND_CAPACITY: usize = 64;
const EVENT_CAPACITY: usize = 64;
const CAPTURES_PER_TRACK: usize = 2;
const MAX_CACHED_TRACKS: usize = 8;
const INDEX_WRITE_SLACK_BITS: usize = 30;

enum Command {
    Motor { side: crate::Side, enabled: bool },
    Seek(TrackAddress),
    NoClick(crate::Side),
    Advance(TrackAddress),
    Write { id: WriteId, request: WriteRequest },
    Shutdown,
}

type CaptureCache = HashMap<TrackAddress, VecDeque<TrackCapture>>;

/// One open physical drive.
///
/// The transport and every mutable hardware fact are owned by a worker
/// thread. Public methods only enqueue bounded commands or inspect snapshots,
/// so normal reads never block the caller on a rotating disk.
pub struct Bridge {
    commands: Sender<Command>,
    events: Receiver<BridgeEvent>,
    status: Arc<Mutex<DriveStatus>>,
    captures: Arc<Mutex<CaptureCache>>,
    selected_port: PortId,
    next_write: u64,
    max_cylinders: u8,
    stall_timeout: Option<Duration>,
    worker: Option<JoinHandle<()>>,
}

impl Bridge {
    /// Opens and probes a physical interface, then starts its controller worker.
    pub fn open(config: &BridgeConfig) -> Result<Self> {
        let mut device = device::open(config)?;
        let selected_port = device.selected_port().clone();
        let initial_status = device.status()?;
        Ok(Self::from_device(
            config.clone(),
            device,
            initial_status,
            selected_port,
        ))
    }

    fn from_device(
        config: BridgeConfig,
        device: Box<dyn Device>,
        initial_status: DriveStatus,
        selected_port: PortId,
    ) -> Self {
        let (command_tx, command_rx) = bounded(COMMAND_CAPACITY);
        let (event_tx, event_rx) = bounded(EVENT_CAPACITY);
        let status = Arc::new(Mutex::new(initial_status));
        let captures = Arc::new(Mutex::new(HashMap::new()));
        let worker_status = Arc::clone(&status);
        let worker_captures = Arc::clone(&captures);
        let stall_timeout = (config.mode == ReadMode::Stalling).then_some(config.stall_timeout);
        let worker = thread::Builder::new()
            .name(format!("fluxbridge-{}", config.driver))
            .spawn(move || {
                Worker::new(
                    config,
                    device,
                    command_rx,
                    event_tx,
                    worker_status,
                    worker_captures,
                )
                .run();
            })
            .expect("FluxBridge worker thread creation must succeed");

        Self {
            commands: command_tx,
            events: event_rx,
            status,
            captures,
            selected_port,
            next_write: 1,
            max_cylinders: initial_status.max_cylinders,
            stall_timeout,
            worker: Some(worker),
        }
    }

    /// Returns the port selected during open.
    pub fn selected_port(&self) -> &PortId {
        &self.selected_port
    }

    /// Returns the latest nonblocking drive-state snapshot.
    pub fn status(&self) -> DriveStatus {
        *lock(&self.status)
    }

    /// Enqueues a motor transition.
    pub fn set_motor(&mut self, side: crate::Side, enabled: bool) -> Result<()> {
        self.enqueue(Command::Motor { side, enabled })
    }

    /// Enqueues a seek and side selection, clamped to the physical mechanism.
    pub fn seek(&mut self, track: TrackAddress) -> Result<()> {
        self.enqueue(Command::Seek(device::clamp_track(
            track,
            self.max_cylinders,
        )))
    }

    /// Requests the no-click step used by track-zero disk-change detection.
    pub fn no_click_step(&mut self, side: crate::Side) -> Result<()> {
        self.enqueue(Command::NoClick(side))
    }

    /// Returns the newest completed capture for a track, if one is ready.
    pub fn read_track(&mut self, track: TrackAddress) -> Result<Option<TrackCapture>> {
        let track = device::clamp_track(track, self.max_cylinders);
        self.enqueue_coalescing(Command::Seek(track))?;
        let capture = || {
            lock(&self.captures)
                .get(&track)
                .and_then(VecDeque::front)
                .cloned()
        };
        if let Some(capture) = capture() {
            return Ok(Some(capture));
        }
        if let Some(timeout) = self.stall_timeout {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                if let Some(capture) = capture() {
                    return Ok(Some(capture));
                }
                if !self.status().working {
                    return Err(Error::WorkerStopped);
                }
                thread::sleep(Duration::from_millis(1));
            }
        }
        Ok(None)
    }

    /// Retires a consumed capture so a later read can return fresh flux.
    pub fn advance_revolution(&mut self, track: TrackAddress) -> Result<()> {
        let track = device::clamp_track(track, self.max_cylinders);
        if let Some(captures) = lock(&self.captures).get_mut(&track) {
            captures.pop_front();
        }
        self.enqueue_coalescing(Command::Advance(track))
    }

    /// Accepts a validated asynchronous write and returns its identifier.
    ///
    /// Completion or failure is delivered by [`Self::poll_event`].
    pub fn submit_write(&mut self, mut request: WriteRequest) -> Result<WriteId> {
        request.track = device::clamp_track(request.track, self.max_cylinders);
        if request.words.is_empty() {
            return Err(Error::InvalidConfig("cannot submit an empty write".into()));
        }
        let available_bits = request
            .words
            .len()
            .checked_mul(16)
            .ok_or_else(|| Error::InvalidConfig("write length overflow".into()))?;
        if request.bit_len == 0 || request.bit_len > available_bits {
            return Err(Error::InvalidConfig(format!(
                "write bit length {} does not fit {} words",
                request.bit_len,
                request.words.len()
            )));
        }
        let bit_len = request.bit_len;
        if bit_len > crate::flux::MAX_TRACK_BITS {
            return Err(Error::TrackTooLarge {
                bits: bit_len,
                limit: crate::flux::MAX_TRACK_BITS,
            });
        }
        if self.status().write_protected {
            return Err(Error::WriteProtected);
        }

        let known_track_bits = lock(&self.captures)
            .get(&request.track)
            .and_then(VecDeque::front)
            .map_or(0, TrackCapture::bit_len);
        if known_track_bits > 0 {
            let from_index = request.start_bit <= INDEX_WRITE_SLACK_BITS
                || request.start_bit + INDEX_WRITE_SLACK_BITS >= known_track_bits;
            let whole_revolution = bit_len + 16 >= known_track_bits;
            if !from_index && !whole_revolution {
                return Err(Error::UnplaceablePartialWrite {
                    start_bit: request.start_bit,
                    track_bits: known_track_bits,
                });
            }
        }

        let id = WriteId(self.next_write);
        self.next_write = self.next_write.wrapping_add(1).max(1);
        self.enqueue(Command::Write { id, request })?;
        Ok(id)
    }

    /// Returns the next pending event without blocking.
    pub fn poll_event(&mut self) -> Option<BridgeEvent> {
        self.events.try_recv().ok()
    }

    fn enqueue(&self, command: Command) -> Result<()> {
        self.commands
            .try_send(command)
            .map_err(|error| match error {
                TrySendError::Full(_) => {
                    Error::InvalidConfig("bridge command queue is full".into())
                }
                TrySendError::Disconnected(_) => Error::WorkerStopped,
            })
    }

    fn enqueue_coalescing(&self, command: Command) -> Result<()> {
        match self.commands.try_send(command) {
            Ok(()) | Err(TrySendError::Full(_)) => Ok(()),
            Err(TrySendError::Disconnected(_)) => Err(Error::WorkerStopped),
        }
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl std::fmt::Debug for Bridge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Bridge")
            .field("selected_port", &self.selected_port)
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

struct Worker {
    config: BridgeConfig,
    device: Box<dyn Device>,
    commands: Receiver<Command>,
    events: Sender<BridgeEvent>,
    status: Arc<Mutex<DriveStatus>>,
    captures: Arc<Mutex<CaptureCache>>,
    target: TrackAddress,
    physical: TrackAddress,
    generation: u64,
    next_status: Instant,
    cache_cursor: u8,
}

impl Worker {
    fn new(
        config: BridgeConfig,
        device: Box<dyn Device>,
        commands: Receiver<Command>,
        events: Sender<BridgeEvent>,
        status: Arc<Mutex<DriveStatus>>,
        captures: Arc<Mutex<CaptureCache>>,
    ) -> Self {
        Self {
            config,
            device,
            commands,
            events,
            status,
            captures,
            target: TrackAddress::default(),
            physical: TrackAddress::default(),
            generation: 0,
            next_status: Instant::now(),
            cache_cursor: 0,
        }
    }

    fn run(mut self) {
        let mut shutdown = false;
        while !shutdown {
            match self.commands.recv_timeout(Duration::from_millis(5)) {
                Ok(command) => shutdown = self.process(command),
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            }
            if shutdown {
                break;
            }
            while let Ok(command) = self.commands.try_recv() {
                if self.process(command) {
                    shutdown = true;
                    break;
                }
            }
            if shutdown {
                break;
            }
            self.refresh_status_if_due();
            self.capture_if_needed();
        }
        let _ = self.device.set_motor(false, true);
        lock(&self.status).working = false;
    }

    fn process(&mut self, command: Command) -> bool {
        let result = match command {
            Command::Motor { side, enabled } => self
                .position(TrackAddress {
                    cylinder: self.target.cylinder,
                    side,
                })
                .and_then(|()| self.device.set_motor(enabled, true))
                .map(|()| lock(&self.status).motor_running = enabled),
            Command::Seek(track) | Command::Advance(track) => {
                self.target = track;
                self.position(track)
            }
            Command::NoClick(side) => self
                .device
                .select_side(side)
                .and_then(|()| self.device.no_click_step()),
            Command::Write { id, request } => {
                self.perform_write(id, request);
                Ok(())
            }
            Command::Shutdown => return true,
        };
        if let Err(error) = result {
            self.disconnect(error);
        }
        false
    }

    fn position(&mut self, track: TrackAddress) -> Result<()> {
        if self.physical.cylinder != track.cylinder {
            self.device.seek(track.cylinder)?;
            self.physical.cylinder = track.cylinder;
        }
        if self.physical.side != track.side {
            self.device.select_side(track.side)?;
            self.physical.side = track.side;
        }
        let mut status = lock(&self.status);
        status.cylinder = self.physical.cylinder;
        status.side = self.physical.side;
        Ok(())
    }

    fn perform_write(&mut self, id: WriteId, request: WriteRequest) {
        let WriteRequest {
            track,
            words,
            bit_len,
            start_bit,
        } = request;
        // Restore both coordinates immediately before every write. Auto-cache
        // may have moved either one since the request was queued.
        let result = self.position(track).and_then(|()| {
            let track_bits = lock(&self.captures)
                .get(&track)
                .and_then(VecDeque::front)
                .map_or(bit_len, TrackCapture::bit_len);
            let write_bits = bit_len;
            let from_index = start_bit <= INDEX_WRITE_SLACK_BITS
                || start_bit + INDEX_WRITE_SLACK_BITS >= track_bits;
            let whole_revolution = write_bits + 16 >= track_bits;
            if !from_index && !whole_revolution {
                return Err(Error::UnplaceablePartialWrite {
                    start_bit,
                    track_bits,
                });
            }
            self.device
                .write_track(&words, write_bits, from_index, self.config.density)
        });

        lock(&self.captures).remove(&track);
        let event = match result {
            Ok(()) => BridgeEvent::WriteCompleted { id, track, bit_len },
            Err(error) => BridgeEvent::WriteFailed { id, track, error },
        };
        let _ = self.events.try_send(event);
    }

    fn capture_if_needed(&mut self) {
        let status = *lock(&self.status);
        if !status.working || !status.motor_running || !status.disk_present {
            return;
        }

        let mut wanted = self.target;
        let target_full = lock(&self.captures)
            .get(&self.target)
            .is_some_and(|captures| captures.len() >= CAPTURES_PER_TRACK);
        if target_full && self.config.auto_cache {
            let max = status.max_cylinders.max(1);
            self.cache_cursor = self.cache_cursor.wrapping_add(1) % max;
            wanted.cylinder = self.cache_cursor;
            if lock(&self.captures)
                .get(&wanted)
                .is_some_and(|captures| !captures.is_empty())
            {
                return;
            }
        } else if target_full {
            return;
        }

        if self.position(wanted).is_err() {
            return;
        }
        let capture = match self
            .device
            .read_track(self.config.mode, self.config.density)
        {
            Ok(capture) => capture,
            Err(Error::Timeout(_)) => return,
            Err(error) => {
                self.disconnect(error);
                return;
            }
        };
        if let Err(error) = device::validate_capture(&capture) {
            self.disconnect(error);
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        let quality = if capture.index_aligned {
            CaptureQuality::IndexAligned
        } else {
            match scan_revolution(&capture.words, capture.bit_len) {
                RevolutionScan::CleanAmigaDos { sectors } => {
                    CaptureQuality::VerifiedAmigaDos { sectors }
                }
                RevolutionScan::DamagedAmigaDos { .. } | RevolutionScan::Unrecognised => {
                    CaptureQuality::Unverified
                }
            }
        };
        let capture = TrackCapture::new(capture.words, capture.bit_len, quality, self.generation);
        let mut cache = lock(&self.captures);
        if cache.len() >= MAX_CACHED_TRACKS
            && !cache.contains_key(&wanted)
            && let Some(key) = cache.keys().copied().find(|key| *key != self.target)
        {
            cache.remove(&key);
        }
        let captures = cache.entry(wanted).or_default();
        if captures.len() >= CAPTURES_PER_TRACK {
            captures.pop_front();
        }
        captures.push_back(capture);
    }

    fn refresh_status_if_due(&mut self) {
        if Instant::now() < self.next_status {
            return;
        }
        self.next_status = Instant::now() + Duration::from_millis(250);
        let old = *lock(&self.status);
        match self.device.status() {
            Ok(mut new) => {
                new.cylinder = self.physical.cylinder;
                new.side = self.physical.side;
                if old.disk_present != new.disk_present {
                    lock(&self.captures).clear();
                    let _ = self.events.try_send(BridgeEvent::DiskChanged {
                        present: new.disk_present,
                    });
                }
                *lock(&self.status) = new;
            }
            Err(error) => self.disconnect(error),
        }
    }

    fn disconnect(&mut self, error: Error) {
        let kind = error.kind();
        lock(&self.status).working = false;
        if matches!(
            kind,
            crate::ErrorKind::Disconnected | crate::ErrorKind::WorkerStopped
        ) {
            let _ = self.events.try_send(BridgeEvent::Disconnected(error));
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DensityMode, DriveType, DriverKind, PortSelection, ReadMode, Side};

    struct FakeDevice {
        port: PortId,
        status: DriveStatus,
        physical: TrackAddress,
        writes: Arc<Mutex<Vec<TrackAddress>>>,
    }

    impl Device for FakeDevice {
        fn selected_port(&self) -> &PortId {
            &self.port
        }

        fn status(&mut self) -> Result<DriveStatus> {
            Ok(self.status)
        }

        fn set_motor(&mut self, enabled: bool, _quick: bool) -> Result<()> {
            self.status.motor_running = enabled;
            self.status.ready = enabled;
            Ok(())
        }

        fn seek(&mut self, cylinder: u8) -> Result<()> {
            self.physical.cylinder = cylinder;
            Ok(())
        }

        fn select_side(&mut self, side: Side) -> Result<()> {
            self.physical.side = side;
            Ok(())
        }

        fn no_click_step(&mut self) -> Result<()> {
            Ok(())
        }

        fn read_track(
            &mut self,
            _mode: ReadMode,
            _density: DensityMode,
        ) -> Result<device::RawCapture> {
            Ok(device::RawCapture {
                words: vec![0xaaaa; 6_250],
                bit_len: 100_000,
                index_aligned: true,
            })
        }

        fn write_track(
            &mut self,
            _words: &[u16],
            _bit_len: usize,
            _from_index: bool,
            _density: DensityMode,
        ) -> Result<()> {
            lock(&self.writes).push(self.physical);
            Ok(())
        }
    }

    fn fake_bridge(auto_cache: bool) -> (Bridge, Arc<Mutex<Vec<TrackAddress>>>) {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let status = DriveStatus {
            ready: true,
            disk_present: true,
            write_protected: false,
            motor_running: true,
            drive_type: DriveType::Dd35,
            ..DriveStatus::default()
        };
        let device = FakeDevice {
            port: PortId::new("fake").unwrap(),
            status,
            physical: TrackAddress::default(),
            writes: Arc::clone(&writes),
        };
        let config = BridgeConfig {
            driver: DriverKind::DrawBridge,
            auto_cache,
            port: PortSelection::Auto,
            ..BridgeConfig::default()
        };
        (
            Bridge::from_device(
                config,
                Box::new(device),
                status,
                PortId::new("fake").unwrap(),
            ),
            writes,
        )
    }

    #[test]
    fn capture_is_nonblocking_and_eventually_available() {
        let (mut bridge, _) = fake_bridge(false);
        let track = TrackAddress::default();
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if let Some(capture) = bridge.read_track(track).unwrap() {
                assert_eq!(capture.bit_len(), 100_000);
                break;
            }
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
    }

    #[test]
    fn write_restores_target_after_auto_cache_move() {
        let (mut bridge, writes) = fake_bridge(true);
        let target = TrackAddress {
            cylinder: 40,
            side: Side::Upper,
        };
        bridge
            .submit_write(WriteRequest {
                track: target,
                words: vec![0xaaaa; 6_250],
                bit_len: 100_000,
                start_bit: 0,
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while lock(&writes).is_empty() {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        assert_eq!(lock(&writes)[0], target);
    }
}
