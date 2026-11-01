//! The live tracker's standby: the camera and the detector come up when the
//! hub wants them (a client is connected or a session is active) and are
//! released once it has not wanted them for [`GRACE`], so the camera stops
//! streaming and the detector's GPU memory is free for other work.

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use device::{Camera, CaptureStats, Frame, Intrinsics};
use tracker::StereoCalib;

use crate::detector::Detector;
use crate::hub::Hub;
use crate::protocol::LiveStatus;

/// How long the camera and detector stay up after the last client leaves.
pub const GRACE: Duration = Duration::from_secs(60);
/// How long to wait before trying again after the camera or detector failed
/// to come up.
const RETRY: Duration = Duration::from_secs(10);
/// How long a poll waits while there is no camera.
const STANDBY_POLL: Duration = Duration::from_millis(50);

/// What the detector thread is told about the engine it runs.
pub enum Engine {
    /// The camera is up: detect with this model and stereo geometry.
    Start {
        detector: Detector,
        calib: StereoCalib,
    },
    /// A promoted model, to replace the running one; dropped in standby.
    Swap(Detector),
    /// The camera is released: drop the detector.
    Release,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Start,
    Stop,
    Wait,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Phase {
    #[default]
    Standby,
    /// Up; `unwanted` is when the hub stopped wanting it.
    Running {
        unwanted: Option<Instant>,
    },
    Failed {
        at: Instant,
    },
}

/// When to bring the camera and detector up and when to release them.
#[derive(Debug, Default)]
pub struct Lifecycle {
    phase: Phase,
}

impl Lifecycle {
    /// What to do now, given whether the hub wants the camera. After
    /// [`Action::Start`], report the outcome with [`Lifecycle::started`].
    pub fn next(&mut self, wanted: bool, now: Instant) -> Action {
        match (self.phase, wanted) {
            (Phase::Standby, true) => Action::Start,
            (Phase::Standby, false) => Action::Wait,
            (Phase::Running { .. }, true) => {
                self.phase = Phase::Running { unwanted: None };
                Action::Wait
            }
            (Phase::Running { unwanted }, false) => {
                let since = unwanted.unwrap_or(now);
                if now.duration_since(since) >= GRACE {
                    self.phase = Phase::Standby;
                    Action::Stop
                } else {
                    self.phase = Phase::Running {
                        unwanted: Some(since),
                    };
                    Action::Wait
                }
            }
            (Phase::Failed { at }, true) if now.duration_since(at) >= RETRY => Action::Start,
            (Phase::Failed { .. }, true) => Action::Wait,
            (Phase::Failed { .. }, false) => {
                self.phase = Phase::Standby;
                Action::Stop
            }
        }
    }

    pub fn started(&mut self, ok: bool, now: Instant) {
        self.phase = if ok {
            Phase::Running { unwanted: None }
        } else {
            Phase::Failed { at: now }
        };
    }
}

pub type Open = Box<dyn FnMut() -> Result<(Box<dyn Camera + Send>, StereoCalib), String> + Send>;
pub type Load = Box<dyn Fn() -> Result<Detector, String> + Send>;

/// A camera that is only open while the hub wants it. Opening it loads the
/// detector first, then the camera, and hands both to the detector thread;
/// releasing it closes the camera and tells the detector thread to drop the
/// detector. While closed, polls return no frame.
pub struct OnDemand {
    open: Open,
    load: Load,
    engine: Sender<Engine>,
    hub: Arc<Hub>,
    status: Arc<Mutex<LiveStatus>>,
    /// For [`Camera::intrinsics`] while the camera is closed.
    size: (u32, u32),
    camera: Option<Box<dyn Camera + Send>>,
    /// Set on release until empty stats replace the camera's last ones.
    released: bool,
    lifecycle: Lifecycle,
}

impl OnDemand {
    pub fn new(
        open: Open,
        load: Load,
        engine: Sender<Engine>,
        hub: Arc<Hub>,
        status: Arc<Mutex<LiveStatus>>,
        size: (u32, u32),
    ) -> Self {
        Self {
            open,
            load,
            engine,
            hub,
            status,
            size,
            camera: None,
            released: false,
            lifecycle: Lifecycle::default(),
        }
    }

    fn set_state(&self, state: &str) {
        if let Ok(mut s) = self.status.lock() {
            s.state = state.into();
        }
    }

    fn start(&mut self) {
        println!("starting the detector and the camera (building the engine if it is not cached, minutes on a first start)");
        self.set_state("starting");
        let begun = Instant::now();
        let up = (self.load)().and_then(|detector| {
            let (camera, calib) = (self.open)()?;
            Ok((detector, camera, calib))
        });
        let ok = match up {
            Ok((detector, camera, calib)) => {
                if let Ok(mut s) = self.status.lock() {
                    s.provider = detector.provider().to_string();
                }
                self.camera = Some(camera);
                let _ = self.engine.send(Engine::Start { detector, calib });
                println!("live in {:.1} s", begun.elapsed().as_secs_f64());
                true
            }
            Err(e) => {
                eprintln!("could not start; retrying in {} s: {e}", RETRY.as_secs());
                self.set_state("failed");
                false
            }
        };
        self.lifecycle.started(ok, Instant::now());
    }

    fn stop(&mut self) {
        if self.camera.take().is_some() {
            self.released = true;
            println!("standby: releasing the camera and the detector");
        }
        let _ = self.engine.send(Engine::Release);
    }
}

impl Camera for OnDemand {
    fn intrinsics(&self) -> Intrinsics {
        match &self.camera {
            Some(c) => c.intrinsics(),
            None => Intrinsics::from_fov(self.size.0, self.size.1, 80.0, 55.0),
        }
    }

    fn set_exposure_us(&mut self, exposure_us: u32) -> Result<(), String> {
        self.camera
            .as_mut()
            .map_or(Ok(()), |c| c.set_exposure_us(exposure_us))
    }

    fn set_gain(&mut self, gain: u32) -> Result<(), String> {
        self.camera.as_mut().map_or(Ok(()), |c| c.set_gain(gain))
    }

    fn set_fps(&mut self, fps: f32) -> Result<(), String> {
        self.camera.as_mut().map_or(Ok(()), |c| c.set_fps(fps))
    }

    fn poll(&mut self, timeout: Duration) -> Result<Option<Frame>, String> {
        match self.lifecycle.next(self.hub.wanted(), Instant::now()) {
            Action::Start => self.start(),
            Action::Stop => self.stop(),
            Action::Wait => {}
        }
        match &mut self.camera {
            Some(c) => c.poll(timeout),
            None => {
                std::thread::sleep(STANDBY_POLL);
                Ok(None)
            }
        }
    }

    fn apply_controls(&mut self, exposure_us: u32, gain: u32) -> Result<(), String> {
        self.camera
            .as_mut()
            .map_or(Ok(()), |c| c.apply_controls(exposure_us, gain))
    }

    fn apply_lights(&mut self, flood: f32, dot: f32) -> Result<(), String> {
        self.camera
            .as_mut()
            .map_or(Ok(()), |c| c.apply_lights(flood, dot))
    }

    fn calibration_json(&self) -> Option<String> {
        self.camera.as_ref().and_then(|c| c.calibration_json())
    }

    fn take_stats(&mut self) -> Option<CaptureStats> {
        match &mut self.camera {
            Some(c) => c.take_stats(),
            None => std::mem::take(&mut self.released).then(CaptureStats::default),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_when_wanted_and_releases_after_the_grace_period() {
        let t0 = Instant::now();
        let mut life = Lifecycle::default();
        assert_eq!(life.next(false, t0), Action::Wait);
        assert_eq!(life.next(true, t0), Action::Start);
        life.started(true, t0);
        assert_eq!(life.next(true, t0), Action::Wait);

        let left = t0 + Duration::from_secs(5);
        assert_eq!(life.next(false, left), Action::Wait);
        assert_eq!(life.next(false, left + GRACE / 2), Action::Wait);
        assert_eq!(life.next(false, left + GRACE), Action::Stop);
        assert_eq!(life.next(false, left + GRACE * 2), Action::Wait);
        assert_eq!(life.next(true, left + GRACE * 2), Action::Start);
    }

    #[test]
    fn coming_back_within_the_grace_period_restarts_it() {
        let t0 = Instant::now();
        let mut life = Lifecycle::default();
        life.next(true, t0);
        life.started(true, t0);
        assert_eq!(life.next(false, t0), Action::Wait);
        assert_eq!(life.next(true, t0 + GRACE / 2), Action::Wait);
        assert_eq!(life.next(false, t0 + GRACE), Action::Wait);
        assert_eq!(life.next(false, t0 + GRACE + GRACE / 2), Action::Wait);
        assert_eq!(life.next(false, t0 + GRACE * 2), Action::Stop);
    }

    #[test]
    fn a_failed_start_waits_before_retrying() {
        let t0 = Instant::now();
        let mut life = Lifecycle::default();
        assert_eq!(life.next(true, t0), Action::Start);
        life.started(false, t0);
        assert_eq!(life.next(true, t0 + RETRY / 2), Action::Wait);
        assert_eq!(life.next(true, t0 + RETRY), Action::Start);
        life.started(false, t0 + RETRY);
        assert_eq!(life.next(false, t0 + RETRY), Action::Stop);
        assert_eq!(life.next(true, t0 + RETRY), Action::Start);
    }
}
