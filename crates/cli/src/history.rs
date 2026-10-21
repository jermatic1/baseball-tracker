//! The tracker page's history: every user with their stats across all
//! their sessions, and every session with its summary. Computed from the
//! store's events when asked.

use serde::Serialize;
use tracker::{summary_lines, Activity, Mode, Store, SummaryLine, TrackerError};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct History {
    pub users: Vec<UserHistory>,
    /// Newest first.
    pub sessions: Vec<SessionHistory>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UserHistory {
    pub id: String,
    pub name: String,
    pub sessions: usize,
    pub hitting: Vec<SummaryLine>,
    pub pitching: Vec<SummaryLine>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionHistory {
    pub id: String,
    pub user: String,
    pub activity: Activity,
    pub mode: Mode,
    pub started: String,
    pub ended: Option<String>,
    pub lines: Vec<SummaryLine>,
}

pub fn build(store: &Store) -> Result<History, TrackerError> {
    let mut users: Vec<UserHistory> = store
        .users()?
        .into_iter()
        .map(|u| UserHistory {
            id: u.id.to_string(),
            name: u.name,
            sessions: 0,
            hitting: Vec::new(),
            pitching: Vec::new(),
        })
        .collect();
    let mut by_user: Vec<(i64, Activity, Vec<tracker::HitRecord>)> = Vec::new();
    let mut sessions = Vec::new();
    for s in store.sessions(None)? {
        let events = store.events(s.id)?;
        sessions.push(SessionHistory {
            id: s.id.to_string(),
            user: s.user.to_string(),
            activity: s.activity,
            mode: s.mode,
            started: s.started.clone(),
            ended: s.ended.clone(),
            lines: summary_lines(s.activity, &events),
        });
        if let Some(u) = users.iter_mut().find(|u| u.id == s.user.to_string()) {
            u.sessions += 1;
        }
        match by_user
            .iter_mut()
            .find(|(u, a, _)| *u == s.user && *a == s.activity)
        {
            Some((_, _, all)) => all.extend(events),
            None => by_user.push((s.user, s.activity, events)),
        }
    }
    for (user, activity, events) in by_user {
        if let Some(u) = users.iter_mut().find(|u| u.id == user.to_string()) {
            let lines = summary_lines(activity, &events);
            match activity {
                Activity::Hitting => u.hitting = lines,
                Activity::Pitching => u.pitching = lines,
            }
        }
    }
    Ok(History { users, sessions })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracker::{EventKind, HitRecord};

    fn hit(speed: f64) -> HitRecord {
        HitRecord {
            clip: "0001".into(),
            kind: EventKind::Hit,
            segment: 0,
            frame_start: 0,
            frame_end: 1,
            start: 0.0,
            exit_velocity: speed,
            launch_angle: 10.0,
            spray_angle: 0.0,
            pitch_velocity: None,
            hit_type: None,
            contact: None,
            samples: 1,
            confident: true,
            hit: None,
            pitch: None,
        }
    }

    #[test]
    fn users_carry_stats_across_their_sessions() {
        let store = Store::in_memory().unwrap();
        let sam = store.add_user("Sam").unwrap();
        let dad = store.add_user("Dad").unwrap();
        let a = store
            .start_session(sam.id, Activity::Hitting, Mode::Practice)
            .unwrap();
        store.add_event(a.id, &hit(50.0)).unwrap();
        store.end_session(a.id).unwrap();
        let b = store
            .start_session(sam.id, Activity::Hitting, Mode::Game)
            .unwrap();
        store.add_event(b.id, &hit(70.0)).unwrap();
        let c = store
            .start_session(dad.id, Activity::Pitching, Mode::Game)
            .unwrap();

        let h = build(&store).unwrap();
        assert_eq!(
            h.sessions.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["3", "2", "1"]
        );
        assert_eq!(h.sessions[2].lines[0].value, "1");
        assert!(h.sessions[0].ended.is_none());
        let sam = &h.users[0];
        assert_eq!((sam.name.as_str(), sam.sessions), ("Sam", 2));
        assert_eq!(sam.hitting[0].value, "2", "balls hit across both sessions");
        assert_eq!(sam.hitting[1].value, "70 MPH");
        assert!(sam.pitching.is_empty());
        let dad = &h.users[1];
        assert_eq!(dad.sessions, 1);
        assert_eq!(dad.pitching[0].value, "0");
        assert_eq!(c.id, 3);
    }
}
