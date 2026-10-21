//! Display-ready stats for a session, computed from its events. Labels fit
//! sixteen characters and values ten, for an 8-pixel TV font.

use serde::{Deserialize, Serialize};

use crate::clips::{Call, EventKind, HitRecord};
use crate::store::Activity;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryLine {
    pub label: String,
    pub value: String,
}

fn line(label: &str, value: String) -> SummaryLine {
    SummaryLine {
        label: label.into(),
        value,
    }
}

fn max(values: impl Iterator<Item = f64>) -> Option<f64> {
    values.fold(None, |m, v| Some(m.map_or(v, |m: f64| m.max(v))))
}

fn mean(values: &[f64]) -> Option<f64> {
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

/// Running numbers for a session, computed from the same events as the
/// summary so the two agree. A value with nothing behind it is `None`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStats {
    pub pitches: usize,
    pub strikes: usize,
    pub avg_pitch_mph: Option<f64>,
    pub top_pitch_mph: Option<f64>,
    pub avg_exit_mph: Option<f64>,
    pub top_exit_mph: Option<f64>,
    pub longest_ft: Option<f64>,
}

fn tenths(v: Option<f64>) -> Option<f64> {
    v.map(|v| (v * 10.0).round() / 10.0)
}

pub fn session_stats(events: &[HitRecord]) -> SessionStats {
    let pitches: Vec<&HitRecord> = events
        .iter()
        .filter(|e| e.kind == EventKind::Pitch)
        .collect();
    let hits: Vec<&HitRecord> = events.iter().filter(|e| e.kind == EventKind::Hit).collect();
    let pitch_speeds: Vec<f64> = pitches.iter().map(|p| p.exit_velocity).collect();
    let exit_speeds: Vec<f64> = hits.iter().map(|h| h.exit_velocity).collect();
    SessionStats {
        pitches: pitches.len(),
        strikes: pitches
            .iter()
            .filter(|p| p.pitch.is_some_and(|p| p.call == Call::Strike))
            .count(),
        avg_pitch_mph: tenths(mean(&pitch_speeds)),
        top_pitch_mph: tenths(max(pitch_speeds.iter().copied())),
        avg_exit_mph: tenths(mean(&exit_speeds)),
        top_exit_mph: tenths(max(exit_speeds.iter().copied())),
        longest_ft: tenths(max(hits
            .iter()
            .filter_map(|h| h.hit.as_ref())
            .map(|f| f.carry))),
    }
}

pub fn summary_lines(activity: Activity, events: &[HitRecord]) -> Vec<SummaryLine> {
    match activity {
        Activity::Hitting => hitting(events),
        Activity::Pitching => pitching(events),
    }
}

fn hitting(events: &[HitRecord]) -> Vec<SummaryLine> {
    let hits: Vec<&HitRecord> = events.iter().filter(|e| e.kind == EventKind::Hit).collect();
    let speeds: Vec<f64> = hits.iter().map(|h| h.exit_velocity).collect();
    let mut out = vec![line("BALLS HIT", hits.len().to_string())];
    if let Some(v) = max(speeds.iter().copied()) {
        out.push(line("MAX EXIT VELO", format!("{v:.0} MPH")));
    }
    if let Some(v) = mean(&speeds) {
        out.push(line("AVG EXIT VELO", format!("{v:.0} MPH")));
    }
    if let Some(v) = max(hits.iter().filter_map(|h| h.hit.as_ref()).map(|f| f.carry)) {
        out.push(line("LONGEST", format!("{v:.0} FT")));
    }
    out
}

fn pitching(events: &[HitRecord]) -> Vec<SummaryLine> {
    let pitches: Vec<&HitRecord> = events
        .iter()
        .filter(|e| e.kind == EventKind::Pitch)
        .collect();
    let speeds: Vec<f64> = pitches.iter().map(|p| p.exit_velocity).collect();
    let mut out = vec![line("PITCHES", pitches.len().to_string())];
    let judged: Vec<Call> = pitches
        .iter()
        .filter_map(|p| p.pitch.map(|p| p.call))
        .collect();
    if !judged.is_empty() {
        let strikes = judged.iter().filter(|c| **c == Call::Strike).count();
        let pct = (strikes as f64 / judged.len() as f64 * 100.0).round();
        out.push(line("STRIKES", format!("{pct:.0}%")));
    }
    if let Some(v) = max(speeds.iter().copied()) {
        out.push(line("TOP SPEED", format!("{v:.0} MPH")));
    }
    if let Some(v) = mean(&speeds) {
        out.push(line("AVG SPEED", format!("{v:.0} MPH")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clips::{HitFlight, Landing, Location, PitchPlate};
    use crate::config::ZoneConfig;

    fn event(kind: EventKind, speed: f64) -> HitRecord {
        HitRecord {
            clip: "0001".into(),
            kind,
            segment: 0,
            frame_start: 0,
            frame_end: 10,
            start: 0.0,
            exit_velocity: speed,
            launch_angle: 10.0,
            spray_angle: 0.0,
            pitch_velocity: None,
            hit_type: None,
            contact: None,
            samples: 10,
            confident: true,
            hit: None,
            pitch: None,
        }
    }

    fn hit(speed: f64, carry: f64) -> HitRecord {
        let mut e = event(EventKind::Hit, speed);
        e.hit = Some(HitFlight {
            carry,
            hang_time: 3.0,
            peak_height: 20.0,
            landing: Landing { x: 0.0, z: carry },
            path: Vec::new(),
        });
        e
    }

    fn pitch(speed: f64, call: Call) -> HitRecord {
        let mut e = event(EventKind::Pitch, speed);
        e.pitch = Some(PitchPlate {
            plate_velocity: speed - 2.0,
            location: Location {
                side: 0.0,
                height: 30.0,
            },
            simulated_location: Location {
                side: 0.0,
                height: 30.0,
            },
            call,
            zone: ZoneConfig::default(),
        });
        e
    }

    fn values(lines: &[SummaryLine]) -> Vec<(&str, &str)> {
        lines
            .iter()
            .map(|l| (l.label.as_str(), l.value.as_str()))
            .collect()
    }

    #[test]
    fn hitting_summary_ignores_pitches_and_bounces() {
        let events = [
            hit(61.0, 142.0),
            hit(35.0, 60.0),
            event(EventKind::Bounce, 20.0),
            pitch(40.0, Call::Strike),
        ];
        assert_eq!(
            values(&summary_lines(Activity::Hitting, &events)),
            vec![
                ("BALLS HIT", "2"),
                ("MAX EXIT VELO", "61 MPH"),
                ("AVG EXIT VELO", "48 MPH"),
                ("LONGEST", "142 FT"),
            ]
        );
    }

    #[test]
    fn pitching_summary_counts_strikes() {
        let events = [
            pitch(44.0, Call::Strike),
            pitch(40.0, Call::Ball),
            pitch(42.0, Call::Strike),
            hit(50.0, 100.0),
        ];
        assert_eq!(
            values(&summary_lines(Activity::Pitching, &events)),
            vec![
                ("PITCHES", "3"),
                ("STRIKES", "67%"),
                ("TOP SPEED", "44 MPH"),
                ("AVG SPEED", "42 MPH"),
            ]
        );
    }

    #[test]
    fn empty_session_still_has_a_count() {
        assert_eq!(
            values(&summary_lines(Activity::Hitting, &[])),
            vec![("BALLS HIT", "0")]
        );
        assert_eq!(
            values(&summary_lines(Activity::Pitching, &[])),
            vec![("PITCHES", "0")]
        );
    }

    #[test]
    fn lines_fit_the_tv() {
        let events = [hit(100.0, 450.0), pitch(100.0, Call::Strike)];
        for activity in [Activity::Hitting, Activity::Pitching] {
            for l in summary_lines(activity, &events) {
                assert!(l.label.len() <= 16, "{}", l.label);
                assert!(l.value.len() <= 10, "{}", l.value);
            }
        }
    }

    #[test]
    fn session_stats_count_both_kinds_and_leave_missing_values_empty() {
        assert_eq!(
            session_stats(&[]),
            SessionStats {
                pitches: 0,
                strikes: 0,
                avg_pitch_mph: None,
                top_pitch_mph: None,
                avg_exit_mph: None,
                top_exit_mph: None,
                longest_ft: None,
            }
        );
        let s = session_stats(&[
            pitch(40.0, Call::Strike),
            pitch(44.25, Call::Ball),
            hit(60.0, 142.5),
            hit(50.0, 100.0),
        ]);
        assert_eq!((s.pitches, s.strikes), (2, 1));
        assert_eq!((s.avg_pitch_mph, s.top_pitch_mph), (Some(42.1), Some(44.3)));
        assert_eq!((s.avg_exit_mph, s.top_exit_mph), (Some(55.0), Some(60.0)));
        assert_eq!(s.longest_ft, Some(142.5));
    }
}
