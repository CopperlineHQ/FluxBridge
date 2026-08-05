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
/// Hard ceiling on readings kept per track. How many are wanted at any moment
/// is decided by [`wanted_depth`]; this only bounds the queue behind it.
const CAPTURES_PER_TRACK: usize = 2;
const MAX_CACHED_TRACKS: usize = 8;
const INDEX_WRITE_SLACK_BITS: usize = 30;

enum Command {
    NoClick(crate::Side),
    Write { id: WriteId, request: WriteRequest },
    Shutdown,
}

/// The caller's latest wish for the mechanism, overwritten rather than queued.
///
/// An emulated machine expresses motor and head state thousands of times a
/// second -- every guest step pulse, every CIA motor write, every poll for the
/// track under the head. Queuing those as commands replays the machine's whole
/// journey against a device that can only move so fast, and a full queue loses
/// whichever command arrives next; the loss of a motor transition leaves the
/// worker's view of the drive permanently wrong. Only the *latest* state can
/// matter to a physical mechanism, so that is all this keeps.
#[derive(Default, Clone, Copy)]
struct Desired {
    motor: Option<(crate::Side, bool)>,
    target: Option<TrackAddress>,
}

/// Everything a worker shares with the [`Bridge`] that owns it.
struct SharedState {
    status: Arc<Mutex<DriveStatus>>,
    captures: Arc<Mutex<CaptureCache>>,
    desired: Arc<Mutex<Desired>>,
    nudge: Receiver<()>,
}

/// Consecutive device errors after which the drive is declared lost.
///
/// A spinning mechanism has weather -- an overflowed capture, a read racing a
/// motor toggle -- and a single failure says nothing. A cable that has been
/// pulled fails every time, which is what this distinguishes.
const MAX_CONSECUTIVE_ERRORS: u32 = 20;

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
    desired: Arc<Mutex<Desired>>,
    /// Wakes the worker the moment a wish changes, so acting on it is not held
    /// for the tail of a poll interval.
    nudge: Sender<()>,
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
        let desired = Arc::new(Mutex::new(Desired::default()));
        let (nudge_tx, nudge_rx) = bounded(1);
        let shared = SharedState {
            status: Arc::clone(&status),
            captures: Arc::clone(&captures),
            desired: Arc::clone(&desired),
            nudge: nudge_rx,
        };
        let stall_timeout = (config.mode == ReadMode::Stalling).then_some(config.stall_timeout);
        let worker = thread::Builder::new()
            .name(format!("fluxbridge-{}", config.driver))
            .spawn(move || {
                Worker::new(config, device, command_rx, event_tx, shared).run();
            })
            .expect("FluxBridge worker thread creation must succeed");

        Self {
            commands: command_tx,
            events: event_rx,
            status,
            captures,
            desired,
            nudge: nudge_tx,
            selected_port,
            next_write: 1,
            max_cylinders: initial_status.max_cylinders,
            stall_timeout,
            worker: Some(worker),
        }
    }

    /// Wakes the worker so a fresh wish is seen now rather than at the end of
    /// its poll interval. A full slot already means a wake-up is on its way.
    fn nudge(&self) {
        let _ = self.nudge.try_send(());
    }

    /// Returns the port selected during open.
    pub fn selected_port(&self) -> &PortId {
        &self.selected_port
    }

    /// Returns the latest nonblocking drive-state snapshot.
    pub fn status(&self) -> DriveStatus {
        *lock(&self.status)
    }

    /// Records the desired motor state, applied by the worker as its next act.
    ///
    /// State rather than a command: an emulated machine toggles the motor as
    /// often as its guest pleases, and only the latest wish can matter to a
    /// physical spindle. Cannot fail and cannot be lost.
    pub fn set_motor(&mut self, side: crate::Side, enabled: bool) -> Result<()> {
        lock(&self.desired).motor = Some((side, enabled));
        self.nudge();
        Ok(())
    }

    /// Records where the head should be, clamped to the physical mechanism.
    ///
    /// State rather than a command, so a guest stepping every three
    /// milliseconds steers the head without replaying its whole journey: the
    /// worker moves to wherever the head belongs *now*, not through every
    /// cylinder it was ever asked for on the way.
    pub fn seek(&mut self, track: TrackAddress) -> Result<()> {
        lock(&self.desired).target = Some(device::clamp_track(track, self.max_cylinders));
        self.nudge();
        Ok(())
    }

    /// Requests the no-click step used by track-zero disk-change detection.
    pub fn no_click_step(&mut self, side: crate::Side) -> Result<()> {
        self.enqueue(Command::NoClick(side))
    }

    /// Returns the newest completed capture for a track, if one is ready.
    pub fn read_track(&mut self, track: TrackAddress) -> Result<Option<TrackCapture>> {
        let track = device::clamp_track(track, self.max_cylinders);
        lock(&self.desired).target = Some(track);
        let capture = || {
            lock(&self.captures)
                .get(&track)
                .and_then(VecDeque::front)
                .cloned()
        };
        if let Some(capture) = capture() {
            return Ok(Some(capture));
        }
        // A miss means someone is actively waiting on this track: worth waking
        // the worker for, where a hit needed nothing from it.
        self.nudge();
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
        // The worker refills any track below its capture depth on its own;
        // naming the track keeps the head there while it does.
        lock(&self.desired).target = Some(track);
        self.nudge();
        Ok(())
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
    desired: Arc<Mutex<Desired>>,
    nudge: Receiver<()>,
    target: TrackAddress,
    physical: TrackAddress,
    positioned: bool,
    failed: bool,
    /// Device errors since the last success. A mechanism mid-spin produces
    /// occasional weather; only an unbroken run of failures means the drive is
    /// really gone.
    consecutive_errors: u32,
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
        shared: SharedState,
    ) -> Self {
        Self {
            config,
            device,
            commands,
            events,
            status: shared.status,
            captures: shared.captures,
            desired: shared.desired,
            nudge: shared.nudge,
            target: TrackAddress::default(),
            physical: TrackAddress::default(),
            positioned: false,
            failed: false,
            consecutive_errors: 0,
            generation: 0,
            next_status: Instant::now(),
            cache_cursor: 0,
        }
    }

    fn run(mut self) {
        let mut shutdown = false;
        // Set when the previous pass took a reading: more work is likely
        // waiting -- the retry behind an unproven capture, or a target that
        // moved while the disk was turning -- so go straight round rather
        // than sleeping on the channels.
        let mut hot = false;
        while !shutdown {
            if !hot {
                crossbeam_channel::select! {
                    recv(self.commands) -> command => match command {
                        Ok(command) => shutdown = self.process(command),
                        Err(_) => break,
                    },
                    recv(self.nudge) -> _ => {}
                    default(Duration::from_millis(5)) => {}
                }
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
            self.apply_desired();
            if self.failed {
                break;
            }
            self.refresh_status_if_due();
            if self.failed {
                break;
            }
            hot = self.capture_if_needed();
            if self.failed {
                break;
            }
        }
        let _ = self.device.set_motor(false, true);
        lock(&self.status).working = false;
    }

    fn process(&mut self, command: Command) -> bool {
        let result = match command {
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
            self.note_error(error);
        }
        self.failed
    }

    /// Applies the caller's latest motor and head wishes.
    ///
    /// Runs at the top of every loop, so a capture is only ever begun with the
    /// freshest view of what the machine wants -- a motor turned off between
    /// loops is seen before the next read would have spun against it.
    fn apply_desired(&mut self) {
        let wish = {
            let mut desired = lock(&self.desired);
            Desired {
                motor: desired.motor.take(),
                target: desired.target.take(),
            }
        };
        if let Some(track) = wish.target {
            self.target = track;
            if let Err(error) = self.position(track) {
                self.note_error(error);
                return;
            }
            self.consecutive_errors = 0;
        }
        if let Some((side, enabled)) = wish.motor {
            let result = self
                .position(TrackAddress {
                    cylinder: self.target.cylinder,
                    side,
                })
                .and_then(|()| self.device.set_motor(enabled, false))
                .map(|()| lock(&self.status).motor_running = enabled);
            match result {
                Ok(()) => self.consecutive_errors = 0,
                Err(error) => self.note_error(error),
            }
        }
    }

    /// Counts a device error, declaring the drive lost only when errors run
    /// unbroken or the device is positively gone.
    fn note_error(&mut self, error: Error) {
        self.consecutive_errors += 1;
        let fatal = matches!(error, Error::Disconnected | Error::WorkerStopped);
        if fatal || self.consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
            self.disconnect(error);
        }
    }

    fn position(&mut self, track: TrackAddress) -> Result<()> {
        if !self.positioned || self.physical.cylinder != track.cylinder {
            self.device.seek(track.cylinder)?;
            self.physical.cylinder = track.cylinder;
        }
        if !self.positioned || self.physical.side != track.side {
            self.device.select_side(track.side)?;
            self.physical.side = track.side;
        }
        self.positioned = true;
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

    /// Captures whatever the cache is short of, returning whether a reading
    /// was actually taken -- the signal to go straight round again rather than
    /// wait for something to ask.
    fn capture_if_needed(&mut self) -> bool {
        let status = *lock(&self.status);
        if !status.working || !status.motor_running || !status.disk_present {
            return false;
        }

        let mut wanted = self.target;
        let target_full = {
            let cache = lock(&self.captures);
            let captures = cache.get(&self.target);
            let depth = wanted_depth(captures.and_then(VecDeque::back));
            captures.is_some_and(|captures| captures.len() >= depth)
        };
        if target_full && self.config.auto_cache {
            let max = status.max_cylinders.max(1);
            self.cache_cursor = self.cache_cursor.wrapping_add(1) % max;
            wanted.cylinder = self.cache_cursor;
            if lock(&self.captures)
                .get(&wanted)
                .is_some_and(|captures| !captures.is_empty())
            {
                return false;
            }
        } else if target_full {
            return false;
        }

        if let Err(error) = self.position(wanted) {
            self.note_error(error);
            return false;
        }
        let capture = match self
            .device
            .read_track(self.config.mode, self.config.density)
        {
            Ok(capture) => capture,
            // No index came round: the platter is stopping, spinning up, or
            // holds no disk. That is a state of the world, not a fault.
            Err(Error::Timeout(_)) => return false,
            Err(error) => {
                self.note_error(error);
                return false;
            }
        };
        if let Err(error) = device::validate_capture(&capture) {
            self.note_error(error);
            return false;
        }
        self.consecutive_errors = 0;
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
        true
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
                self.consecutive_errors = 0;
            }
            Err(error) => self.note_error(error),
        }
    }

    fn disconnect(&mut self, error: Error) {
        self.failed = true;
        lock(&self.status).working = false;
        let _ = self.events.try_send(BridgeEvent::Disconnected(error));
    }
}

/// How many readings of a track are worth holding, given the newest one.
///
/// A reading that cannot be turned under the head twice is consumed and
/// advanced past, so its successor is stocked ahead: the retry is already in
/// hand when the consumer asks for it. A replayable reading is kept by the
/// consumer for as long as it is wanted, and a second would almost always go
/// unread -- while the capture taking it sat in front of whichever track the
/// machine asked for next, uninterruptible once begun. Measured over a
/// Workbench boot, those speculative readings were discarded rather than
/// served about nineteen times in twenty, at a capture window of dead time
/// apiece.
fn wanted_depth(newest: Option<&TrackCapture>) -> usize {
    match newest {
        Some(capture) if !capture.quality().reusable() => CAPTURES_PER_TRACK,
        _ => 1,
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
        calls: Arc<Mutex<Vec<DeviceCall>>>,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum DeviceCall {
        Motor { enabled: bool, quick: bool },
        Seek(u8),
        Side(Side),
    }

    type FakeBridge = (
        Bridge,
        Arc<Mutex<Vec<TrackAddress>>>,
        Arc<Mutex<Vec<DeviceCall>>>,
    );

    impl Device for FakeDevice {
        fn selected_port(&self) -> &PortId {
            &self.port
        }

        fn status(&mut self) -> Result<DriveStatus> {
            Ok(self.status)
        }

        fn set_motor(&mut self, enabled: bool, quick: bool) -> Result<()> {
            lock(&self.calls).push(DeviceCall::Motor { enabled, quick });
            self.status.motor_running = enabled;
            self.status.ready = enabled;
            Ok(())
        }

        fn seek(&mut self, cylinder: u8) -> Result<()> {
            lock(&self.calls).push(DeviceCall::Seek(cylinder));
            self.physical.cylinder = cylinder;
            Ok(())
        }

        fn select_side(&mut self, side: Side) -> Result<()> {
            lock(&self.calls).push(DeviceCall::Side(side));
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

    fn fake_bridge(auto_cache: bool) -> FakeBridge {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let calls = Arc::new(Mutex::new(Vec::new()));
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
            calls: Arc::clone(&calls),
        };
        let config = BridgeConfig {
            driver: DriverKind::DrawBridge,
            auto_cache,
            port: PortSelection::Auto,
            ..BridgeConfig::default()
        };
        let bridge = Bridge::from_device(
            config,
            Box::new(device),
            status,
            PortId::new("fake").unwrap(),
        );
        (bridge, writes, calls)
    }

    #[test]
    fn capture_is_nonblocking_and_eventually_available() {
        let (mut bridge, _, _) = fake_bridge(false);
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
        let (mut bridge, writes, _) = fake_bridge(true);
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

    #[test]
    fn first_position_selects_both_coordinates_and_motor_uses_spin_up_delay() {
        let (mut bridge, _, calls) = fake_bridge(false);
        bridge.set_motor(Side::Lower, true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let calls = lock(&calls);
            if calls
                .iter()
                .any(|call| matches!(call, DeviceCall::Motor { enabled: true, .. }))
            {
                assert!(calls.contains(&DeviceCall::Seek(0)));
                assert!(calls.contains(&DeviceCall::Side(Side::Lower)));
                assert!(calls.contains(&DeviceCall::Motor {
                    enabled: true,
                    quick: false,
                }));
                break;
            }
            drop(calls);
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
    }

    #[test]
    fn motor_and_seek_are_state_and_survive_any_flood() {
        let (commands, _command_rx) = bounded(1);
        let (_event_tx, events) = bounded(1);
        let (nudge_tx, _nudge_rx) = bounded(1);
        let desired = Arc::new(Mutex::new(Desired::default()));
        let mut bridge = Bridge {
            commands,
            events,
            status: Arc::new(Mutex::new(DriveStatus::default())),
            captures: Arc::new(Mutex::new(HashMap::new())),
            desired: Arc::clone(&desired),
            nudge: nudge_tx,
            selected_port: PortId::new("fake").unwrap(),
            next_write: 1,
            max_cylinders: 80,
            stall_timeout: None,
            worker: None,
        };

        // A guest expresses these thousands of times a second; none may fail
        // and only the latest matters.
        for cylinder in 0..80 {
            assert!(
                bridge
                    .seek(TrackAddress {
                        cylinder,
                        side: Side::Upper,
                    })
                    .is_ok()
            );
            assert!(bridge.set_motor(Side::Lower, cylinder % 2 == 0).is_ok());
        }
        let wish = *lock(&desired);
        assert_eq!(
            wish.target,
            Some(TrackAddress {
                cylinder: 79,
                side: Side::Upper,
            })
        );
        assert_eq!(wish.motor, Some((Side::Lower, false)));
    }
}
