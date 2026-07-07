//! Shared liveness state for the Socket Mode bridge.
//!
//! Today a stalled or silently-dropped connection is invisible: nothing
//! records when a frame last arrived, and the server's `/api/health` knows
//! nothing about the bridge. [`BridgeHealth`] is a small, cheaply-cloned
//! handle the inbound loop threads through `connect_once`/`pump_connection`
//! to record connect/frame/disconnect events, plus a pure [`HealthState::snapshot`]
//! (exposed via [`BridgeHealth::snapshot`]) that turns those events into a
//! stale/live verdict without touching the wall clock itself — the caller
//! supplies `now`, so the decision is deterministic and testable.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long since the last frame (of any kind, including pings) before the
/// bridge is considered stale. Set >= the pump loop's `IDLE_TIMEOUT` (45s) so
/// a normal idle-timeout reconnect cycle doesn't itself flap the health log.
pub const STALENESS_THRESHOLD: Duration = Duration::from_secs(60);

/// A point-in-time read of the bridge's liveness, derived from a `HealthState`
/// and an explicit `now` — see [`BridgeHealth::snapshot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HealthSnapshot {
    pub connected: bool,
    pub secs_since_frame: Option<u64>,
    pub stale: bool,
}

/// Raw liveness state, guarded by `BridgeHealth`'s mutex.
#[derive(Debug, Clone, Copy, Default)]
struct HealthState {
    connected: bool,
    last_frame_at: Option<Instant>,
    connect_count: u64,
}

impl HealthState {
    /// Pure: never calls `Instant::now()`, so tests can pass explicit values
    /// and assert on the result without any reliance on real elapsed time.
    fn snapshot(&self, now: Instant) -> HealthSnapshot {
        let secs_since_frame = self
            .last_frame_at
            .map(|at| now.saturating_duration_since(at).as_secs());
        let stale = match self.last_frame_at {
            Some(at) => now.saturating_duration_since(at) > STALENESS_THRESHOLD,
            None => true,
        };
        HealthSnapshot {
            connected: self.connected,
            secs_since_frame,
            stale,
        }
    }
}

/// Shared, thread-safe liveness handle. Cloning shares the same underlying
/// state (an `Arc<Mutex<_>>`), so the periodic health-log task and the pump
/// loop observe the same connect/frame/disconnect events.
#[derive(Clone, Default)]
pub struct BridgeHealth(Arc<Mutex<HealthState>>);

impl BridgeHealth {
    pub fn new() -> Self {
        Self::default()
    }

    /// Call when `connect_once` establishes the socket: marks connected, bumps
    /// the connect counter, and stamps a frame now (a fresh connection is live).
    pub fn record_connected(&self) {
        let mut state = self.0.lock().unwrap();
        state.connected = true;
        state.connect_count += 1;
        state.last_frame_at = Some(Instant::now());
    }

    /// Call on every frame received inside `pump_connection` (text, ping, pong,
    /// binary — anything that proves the socket is still alive).
    pub fn record_frame(&self) {
        self.0.lock().unwrap().last_frame_at = Some(Instant::now());
    }

    /// Call when a connection ends: stall, disconnect frame, close, or error.
    pub fn record_disconnected(&self) {
        self.0.lock().unwrap().connected = false;
    }

    /// Number of times `record_connected` has fired, i.e. how many times the
    /// bridge has (re)established the socket.
    pub fn connect_count(&self) -> u64 {
        self.0.lock().unwrap().connect_count
    }

    /// See [`HealthState::snapshot`] — pure given `now`.
    pub fn snapshot(&self, now: Instant) -> HealthSnapshot {
        self.0.lock().unwrap().snapshot(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_reports_stale_after_threshold() {
        let now = Instant::now();
        let state = HealthState {
            connected: true,
            last_frame_at: Some(now - (STALENESS_THRESHOLD + Duration::from_secs(1))),
            connect_count: 1,
        };
        assert!(state.snapshot(now).stale);
    }

    #[test]
    fn health_reports_live_within_threshold() {
        let now = Instant::now();
        let state = HealthState {
            connected: true,
            last_frame_at: Some(now - (STALENESS_THRESHOLD / 2)),
            connect_count: 1,
        };
        let snap = state.snapshot(now);
        assert!(!snap.stale);
        assert!(snap.connected);
    }
}
