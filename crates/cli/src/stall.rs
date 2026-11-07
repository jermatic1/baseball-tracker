//! Logs for falling behind the camera: a per-frame stage breakdown printed
//! when a frame runs long, a count of frames a full queue turned away, and
//! locks held long enough to stall the capture thread.

use std::fmt::{self, Write};
use std::time::{Duration, Instant};

/// Two frame periods at 100 fps. The queues absorb one frame this slow; a
/// run of them is what drops frames.
pub const SLOW: Duration = Duration::from_millis(20);
/// The capture thread waits on the capture state lock, so holding it this
/// long delays frames.
const SLOW_HOLD: Duration = Duration::from_millis(5);
/// The shortest gap between repeated logs from one place.
const QUIET: Duration = Duration::from_secs(1);

/// Times the stages of one frame's work.
pub struct Stages {
    what: &'static str,
    start: Instant,
    last: Instant,
    marks: Vec<(&'static str, Duration)>,
    logged: Option<Instant>,
    skipped: u32,
}

impl Stages {
    pub fn new(what: &'static str) -> Self {
        let now = Instant::now();
        Self {
            what,
            start: now,
            last: now,
            marks: Vec::with_capacity(8),
            logged: None,
            skipped: 0,
        }
    }

    pub fn begin(&mut self) {
        let now = Instant::now();
        self.start = now;
        self.last = now;
        self.marks.clear();
    }

    /// Ends the stage running since the last mark.
    pub fn mark(&mut self, stage: &'static str) {
        let now = Instant::now();
        self.marks.push((stage, now - self.last));
        self.last = now;
    }

    /// Logs the breakdown when the frame took longer than `SLOW`, at most
    /// once a second, counting the slow frames in between.
    pub fn check(&mut self, detail: fmt::Arguments) {
        let total = self.last - self.start;
        if total < SLOW {
            return;
        }
        if self.logged.is_some_and(|t| t.elapsed() < QUIET) {
            self.skipped += 1;
            return;
        }
        let mut line = format!("{}: slow frame {:.1} ms (", self.what, ms(total));
        for (i, (stage, d)) in self.marks.iter().enumerate() {
            let sep = if i == 0 { "" } else { ", " };
            let _ = write!(line, "{sep}{stage} {:.1}", ms(*d));
        }
        line.push(')');
        if detail.as_str() != Some("") {
            let _ = write!(line, " {detail}");
        }
        if self.skipped > 0 {
            let _ = write!(line, "; {} more since the last report", self.skipped);
        }
        eprintln!("{line}");
        self.logged = Some(Instant::now());
        self.skipped = 0;
    }
}

/// Counts frames a full queue turned away, logging when a run of them starts
/// and, a second later, how many it lost.
pub struct Drops {
    what: &'static str,
    count: u64,
    since: Option<Instant>,
}

impl Drops {
    pub fn new(what: &'static str) -> Self {
        Self {
            what,
            count: 0,
            since: None,
        }
    }

    pub fn record(&mut self, accepted: bool) {
        if !accepted {
            if self.since.is_none() {
                eprintln!("{}: queue full, dropping frames", self.what);
                self.since = Some(Instant::now());
            }
            self.count += 1;
        }
        if let Some(since) = self.since.filter(|t| t.elapsed() >= QUIET) {
            eprintln!(
                "{}: dropped {} frames in {:.1} s",
                self.what,
                self.count,
                since.elapsed().as_secs_f64()
            );
            self.since = None;
            self.count = 0;
        }
    }
}

/// Logs when a lock the capture thread needs was held since `since` for
/// longer than it can wait.
pub fn held(what: &str, since: Instant) {
    let d = since.elapsed();
    if d >= SLOW_HOLD {
        eprintln!("{what}: held the capture lock {:.1} ms", ms(d));
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}
