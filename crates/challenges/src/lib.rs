//! The challenges game: pitching challenges. A user pitches to a lineup of
//! CPU batters, each with a goal to show, and every pitch gets a verdict
//! and the batter's response. The front end flies any batted ball itself
//! and reports each plate appearance's result. Bodies travel in the core's
//! game envelope; `docs/challenges.md` in the simulator repository has them.

use std::fmt;
use std::fs;
use std::path::Path;

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracker::geom::launch_velocity;
use tracker::{
    flight, Activity, Call, EventKind, Game, HitRecord, HitType, Rng, Session, SummaryLine, User,
};

const EXAMPLE: &str = include_str!("../example.toml");
const PAGE: &str = include_str!("page.html");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Hand {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Height {
    Short,
    Average,
    Tall,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Build {
    Slim,
    Average,
    Heavy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Skin {
    Light,
    Medium,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stance {
    Open,
    Square,
    Closed,
}

/// A CPU batter: what the front end draws, and how it responds to a pitch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Batter {
    pub id: String,
    pub name: String,
    pub number: u32,
    pub bats: Hand,
    pub throws: Hand,
    pub height: Height,
    pub build: Build,
    pub skin: Skin,
    pub stance: Stance,
    /// Chance a swing at a strike makes contact.
    #[serde(default = "default_contact")]
    pub contact: f64,
    /// Typical exit velocity, mph.
    #[serde(default = "default_power")]
    pub power: f64,
}

fn default_contact() -> f64 {
    0.6
}

fn default_power() -> f64 {
    60.0
}

/// What a batter looks like on the wire: the roster entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RosterEntry {
    pub id: String,
    pub name: String,
    pub number: u32,
    pub bats: Hand,
    pub throws: Hand,
    pub height: Height,
    pub build: Build,
    pub skin: Skin,
    pub stance: Stance,
}

impl Batter {
    fn entry(&self) -> RosterEntry {
        RosterEntry {
            id: self.id.clone(),
            name: self.name.clone(),
            number: self.number,
            bats: self.bats,
            throws: self.throws,
            height: self.height,
            build: self.build,
            skin: self.skin,
            stance: self.stance,
        }
    }
}

/// Where a pitch has to go. Thirds of the zone; inside and outside follow
/// the batter's hands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    Any,
    Strike,
    High,
    Low,
    Inside,
    Outside,
    Middle,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Goal {
    pub id: String,
    /// Shown on the TV, so short and in capitals.
    pub text: String,
    pub target: Target,
    /// Release speed the pitch must reach, mph.
    #[serde(default)]
    pub min_velocity: Option<f64>,
}

impl Goal {
    /// Whether a pitch did what the goal asked.
    pub fn met(&self, bats: Hand, record: &HitRecord) -> bool {
        let Some(p) = record.pitch else {
            return false;
        };
        if self
            .min_velocity
            .is_some_and(|min| p.release_velocity < min)
        {
            return false;
        }
        let strike = p.call == Call::Strike;
        let z = p.zone;
        let sx = p.simulated_location.side / (z.width / 2.0);
        let hy = (p.simulated_location.height - z.bottom) / (z.top - z.bottom);
        let third = 1.0 / 3.0;
        let toward_batter = match bats {
            Hand::Right => -sx,
            Hand::Left => sx,
        };
        match self.target {
            Target::Any => true,
            Target::Strike => strike,
            Target::High => strike && hy >= 2.0 * third,
            Target::Low => strike && hy <= third,
            Target::Inside => strike && toward_batter >= third,
            Target::Outside => strike && toward_batter <= -third,
            Target::Middle => strike && sx.abs() < third && hy > third && hy < 2.0 * third,
        }
    }
}

/// A lineup for a user, or for everyone without one of their own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    #[serde(default)]
    pub user: Option<String>,
    pub batters: Vec<String>,
    pub goals: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChallengesConfig {
    pub batters: Vec<Batter>,
    pub goals: Vec<Goal>,
    pub plans: Vec<Plan>,
}

#[derive(Debug)]
pub struct ConfigError(String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "challenges config: {}", self.0)
    }
}

impl ChallengesConfig {
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let cfg: Self = toml::from_str(text).map_err(|e| ConfigError(e.to_string()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn example() -> Self {
        Self::parse(EXAMPLE).expect("the example config is valid")
    }

    fn validate(&self) -> Result<(), ConfigError> {
        for plan in &self.plans {
            let who = plan.user.as_deref().unwrap_or("everyone");
            if plan.batters.is_empty() || plan.goals.is_empty() {
                return Err(ConfigError(format!(
                    "the plan for {who} needs batters and goals"
                )));
            }
            for id in &plan.batters {
                if !self.batters.iter().any(|b| &b.id == id) {
                    return Err(ConfigError(format!("plan for {who}: no batter {id}")));
                }
            }
            for id in &plan.goals {
                if !self.goals.iter().any(|g| &g.id == id) {
                    return Err(ConfigError(format!("plan for {who}: no goal {id}")));
                }
            }
        }
        for b in &self.batters {
            if !(0.0..=1.0).contains(&b.contact) {
                return Err(ConfigError(format!("batter {}: contact is a chance", b.id)));
            }
        }
        Ok(())
    }

    /// The user's own plan, else the one for everyone.
    fn plan_for(&self, user: &str) -> Option<&Plan> {
        self.plans
            .iter()
            .find(|p| p.user.as_deref() == Some(user))
            .or_else(|| self.plans.iter().find(|p| p.user.is_none()))
    }

    fn lineup(&self, user: &str) -> Vec<(Batter, Goal)> {
        let Some(plan) = self.plan_for(user) else {
            return Vec::new();
        };
        let batter = |id: &str| self.batters.iter().find(|b| b.id == id).cloned();
        let goal = |id: &str| self.goals.iter().find(|g| g.id == id).cloned();
        plan.batters
            .iter()
            .enumerate()
            .filter_map(|(i, b)| Some((batter(b)?, goal(&plan.goals[i % plan.goals.len()])?)))
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SwingKind {
    Take,
    Miss,
    Foul,
    Contact,
}

/// Bodies to the front end.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Out {
    /// The session's batters in order.
    Roster { batters: Vec<RosterEntry> },
    /// Who is up and the goal to show.
    Batter { id: String, goal: String },
    /// Whether the pitch met the goal, with a short text to show.
    Verdict { met: bool, text: String },
    /// What the batter did with the pitch; `record` is the batted ball on
    /// contact, shaped like a hit event.
    Swing {
        swing: SwingKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        record: Option<Box<HitRecord>>,
    },
}

/// Bodies from the front end.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum In {
    /// The plate appearance is over.
    Result {
        result: String,
        #[serde(default)]
        runs: Option<u32>,
    },
}

/// The lineup of the active pitching session and how it is going.
struct Live {
    session: i64,
    lineup: Vec<(Batter, Goal)>,
    at: usize,
    judged: usize,
    met: usize,
}

impl Live {
    fn current(&self) -> &(Batter, Goal) {
        &self.lineup[self.at % self.lineup.len()]
    }

    fn batter_body(&self) -> Out {
        let (batter, goal) = self.current();
        Out::Batter {
            id: batter.id.clone(),
            goal: goal.text.clone(),
        }
    }

    fn roster_body(&self) -> Out {
        Out::Roster {
            batters: self.lineup.iter().map(|(b, _)| b.entry()).collect(),
        }
    }
}

pub struct Challenges {
    config: ChallengesConfig,
    db: Connection,
    rng: Rng,
    live: Option<Live>,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS challenges_results (
    id INTEGER PRIMARY KEY,
    session INTEGER NOT NULL,
    batter TEXT NOT NULL,
    goal TEXT NOT NULL,
    result TEXT NOT NULL,
    runs INTEGER,
    created TEXT NOT NULL DEFAULT (datetime('now'))
);
";

impl Challenges {
    /// Load `challenges.toml` from the data directory, writing the example
    /// first if there is none, and keep results in the tracker's database.
    pub fn load(data: &Path) -> Result<Self, String> {
        let path = data.join("challenges.toml");
        if !path.exists() {
            fs::write(&path, EXAMPLE).map_err(|e| e.to_string())?;
        }
        let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let config = ChallengesConfig::parse(&text).map_err(|e| e.to_string())?;
        let db = Connection::open(data.join("tracker.db")).map_err(|e| e.to_string())?;
        Self::with(config, db)
    }

    pub fn with(config: ChallengesConfig, db: Connection) -> Result<Self, String> {
        db.execute_batch(SCHEMA).map_err(|e| e.to_string())?;
        Ok(Self {
            config,
            db,
            rng: Rng::seeded(),
            live: None,
        })
    }

    fn bodies(out: &[Out]) -> Vec<Value> {
        out.iter()
            .filter_map(|o| serde_json::to_value(o).ok())
            .collect()
    }

    /// How the batter responds to a pitch.
    fn swing(&mut self, batter: &Batter, pitch: &HitRecord) -> Out {
        let strike = pitch.pitch.is_some_and(|p| p.call == Call::Strike);
        let swings = self.rng.chance(if strike { 0.85 } else { 0.25 });
        if !swings {
            return Out::Swing {
                swing: SwingKind::Take,
                record: None,
            };
        }
        let contact = batter.contact * if strike { 1.0 } else { 0.4 };
        if !self.rng.chance(contact) {
            return Out::Swing {
                swing: SwingKind::Miss,
                record: None,
            };
        }
        if self.rng.chance(0.35) {
            return Out::Swing {
                swing: SwingKind::Foul,
                record: None,
            };
        }
        let exit_velocity = (batter.power + 10.0 * self.rng.normal()).clamp(20.0, 105.0);
        let launch_angle = self.rng.between(-15.0, 45.0);
        let pull = match batter.bats {
            Hand::Right => -10.0,
            Hand::Left => 10.0,
        };
        let spray_angle = (self.rng.between(-35.0, 35.0) + pull).clamp(-45.0, 45.0);
        let v = launch_velocity(exit_velocity, launch_angle, spray_angle);
        let hit = flight::predict([0.0, 0.9, 0.0], v, flight::air_density(0.0));
        Out::Swing {
            swing: SwingKind::Contact,
            record: Some(Box::new(HitRecord {
                clip: pitch.clip.clone(),
                kind: EventKind::Hit,
                segment: 0,
                frame_start: pitch.frame_end,
                frame_end: pitch.frame_end,
                start: pitch.start,
                exit_velocity,
                launch_angle,
                spray_angle,
                pitch_velocity: Some(pitch.exit_velocity),
                hit_type: Some(HitType::from_launch_deg(launch_angle)),
                contact: None,
                samples: 0,
                confident: true,
                hit: Some(hit),
                pitch: None,
            })),
        }
    }
}

impl Game for Challenges {
    fn name(&self) -> &'static str {
        "challenges"
    }

    fn page(&self) -> Option<&'static str> {
        Some(PAGE)
    }

    fn welcome(&self) -> Vec<Value> {
        match &self.live {
            Some(live) => Self::bodies(&[live.roster_body(), live.batter_body()]),
            None => Vec::new(),
        }
    }

    fn session_started(&mut self, session: &Session, user: &User) -> Vec<Value> {
        self.live = None;
        if session.activity != Activity::Pitching {
            return Vec::new();
        }
        let lineup = self.config.lineup(&user.name);
        if lineup.is_empty() {
            return Vec::new();
        }
        let live = Live {
            session: session.id,
            lineup,
            at: 0,
            judged: 0,
            met: 0,
        };
        let out = Self::bodies(&[live.roster_body(), live.batter_body()]);
        self.live = Some(live);
        out
    }

    fn session_ended(&mut self, session: &Session) -> Vec<SummaryLine> {
        match self.live.take() {
            Some(live) if live.session == session.id && live.judged > 0 => vec![SummaryLine {
                label: "GOALS MET".into(),
                value: format!("{} OF {}", live.met, live.judged),
            }],
            _ => Vec::new(),
        }
    }

    fn event(&mut self, session: &Session, record: &HitRecord) -> Vec<Value> {
        if record.kind != EventKind::Pitch || record.pitch.is_none() {
            return Vec::new();
        }
        let Some(live) = self.live.as_mut().filter(|l| l.session == session.id) else {
            return Vec::new();
        };
        let (batter, goal) = live.current().clone();
        let met = goal.met(batter.bats, record);
        live.judged += 1;
        live.met += usize::from(met);
        let verdict = Out::Verdict {
            met,
            text: if met { "GOAL MET" } else { "MISSED" }.into(),
        };
        let swing = self.swing(&batter, record);
        Self::bodies(&[verdict, swing])
    }

    fn message(&mut self, body: Value) -> Result<Vec<Value>, String> {
        let body: In = serde_json::from_value(body).map_err(|e| format!("challenges: {e}"))?;
        let Some(live) = self.live.as_mut() else {
            return Err("no pitching session".into());
        };
        match body {
            In::Result { result, runs } => {
                let result = result.trim().to_lowercase();
                if result.is_empty() {
                    return Err("result needs a label".into());
                }
                let (batter, goal) = live.current();
                self.db
                    .execute(
                        "INSERT INTO challenges_results (session, batter, goal, result, runs) \
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![live.session, batter.id, goal.id, result, runs],
                    )
                    .map_err(|e| e.to_string())?;
                live.at = (live.at + 1) % live.lineup.len();
                Ok(Self::bodies(&[live.batter_body()]))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracker::{Location, Mode, PitchPlate, ZoneConfig};

    fn pitch(side: f64, height: f64, velocity: f64) -> HitRecord {
        let zone = ZoneConfig::default();
        let location = Location { side, height };
        HitRecord {
            clip: "0001".into(),
            kind: EventKind::Pitch,
            segment: 0,
            frame_start: 0,
            frame_end: 10,
            start: 1.0,
            exit_velocity: velocity,
            launch_angle: 0.0,
            spray_angle: 0.0,
            pitch_velocity: None,
            hit_type: None,
            contact: None,
            samples: 10,
            confident: true,
            hit: None,
            pitch: Some(PitchPlate {
                release_velocity: velocity,
                plate_velocity: velocity - 2.0,
                location,
                simulated_location: location,
                call: if zone.contains(side, height) {
                    Call::Strike
                } else {
                    Call::Ball
                },
                zone,
            }),
        }
    }

    fn goal(target: Target) -> Goal {
        Goal {
            id: "g".into(),
            text: "G".into(),
            target,
            min_velocity: None,
        }
    }

    fn session(id: i64, activity: Activity) -> Session {
        Session {
            id,
            user: 1,
            activity,
            mode: Mode::Game,
            started: String::new(),
            ended: None,
        }
    }

    fn user(name: &str) -> User {
        User {
            id: 1,
            name: name.into(),
        }
    }

    fn game() -> Challenges {
        let mut g = Challenges::with(
            ChallengesConfig::example(),
            Connection::open_in_memory().unwrap(),
        )
        .unwrap();
        g.rng = Rng::with_seed(7);
        g
    }

    #[test]
    fn example_config_builds_a_lineup_and_prefers_a_users_own_plan() {
        let mut cfg = ChallengesConfig::example();
        let lineup = cfg.lineup("Sam");
        assert_eq!(lineup.len(), 3);
        assert_eq!(lineup[0].0.id, "slugger");
        assert_eq!(lineup[0].1.id, "high");
        cfg.plans.push(Plan {
            user: Some("Sam".into()),
            batters: vec!["slap".into(), "rookie".into()],
            goals: vec!["heat".into()],
        });
        let sam = cfg.lineup("Sam");
        assert_eq!(sam.len(), 2);
        assert!(sam.iter().all(|(_, g)| g.id == "heat"), "goals repeat");
        assert_eq!(cfg.lineup("Dad")[0].0.id, "slugger");
    }

    #[test]
    fn config_rejects_dangling_references() {
        let bad = EXAMPLE.replace(
            "goals = [\"high\", \"low\", \"heat\"]",
            "goals = [\"nope\"]",
        );
        assert!(ChallengesConfig::parse(&bad).is_err());
        let empty = EXAMPLE.replace("goals = [\"high\", \"low\", \"heat\"]", "goals = []");
        assert!(ChallengesConfig::parse(&empty).is_err());
    }

    #[test]
    fn goals_are_judged_in_thirds_and_by_hand() {
        let high = pitch(0.0, 39.0, 40.0);
        let low = pitch(0.0, 21.0, 40.0);
        let middle = pitch(0.0, 30.0, 40.0);
        let first_base_side = pitch(7.0, 30.0, 40.0);
        let ball = pitch(12.0, 30.0, 40.0);
        assert!(goal(Target::High).met(Hand::Right, &high));
        assert!(!goal(Target::High).met(Hand::Right, &low));
        assert!(goal(Target::Low).met(Hand::Right, &low));
        assert!(goal(Target::Middle).met(Hand::Right, &middle));
        assert!(!goal(Target::Middle).met(Hand::Right, &high));
        assert!(goal(Target::Strike).met(Hand::Right, &high));
        assert!(!goal(Target::Strike).met(Hand::Right, &ball));
        assert!(goal(Target::Any).met(Hand::Right, &ball));
        // Toward first base is away from a right-handed batter and in on a
        // left-handed one.
        assert!(goal(Target::Outside).met(Hand::Right, &first_base_side));
        assert!(!goal(Target::Inside).met(Hand::Right, &first_base_side));
        assert!(goal(Target::Inside).met(Hand::Left, &first_base_side));
        assert!(
            !goal(Target::Outside).met(Hand::Right, &ball),
            "a ball is not a corner"
        );
        let heat = Goal {
            min_velocity: Some(45.0),
            ..goal(Target::Strike)
        };
        assert!(!heat.met(Hand::Right, &middle));
        assert!(heat.met(Hand::Right, &pitch(0.0, 30.0, 50.0)));
    }

    #[test]
    fn a_pitching_session_runs_the_lineup() {
        let mut g = game();
        assert!(g
            .session_started(&session(1, Activity::Hitting), &user("Sam"))
            .is_empty());
        assert!(g.welcome().is_empty());

        let out = g.session_started(&session(2, Activity::Pitching), &user("Sam"));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["type"], "roster");
        assert_eq!(out[0]["batters"].as_array().unwrap().len(), 3);
        assert!(
            out[0]["batters"][0].get("contact").is_none(),
            "ratings stay here"
        );
        assert_eq!(out[1]["type"], "batter");
        assert_eq!(out[1]["id"], "slugger");
        assert_eq!(out[1]["goal"], "THROW HIGH");
        assert_eq!(g.welcome().len(), 2);

        let out = g.event(&session(2, Activity::Pitching), &pitch(0.0, 39.0, 40.0));
        assert_eq!(out[0]["type"], "verdict");
        assert_eq!(out[0]["met"], true);
        assert_eq!(out[1]["type"], "swing");
        assert!(["take", "miss", "foul", "contact"].contains(&out[1]["swing"].as_str().unwrap()));
        assert!(
            g.event(&session(2, Activity::Pitching), &pitch(0.0, 21.0, 40.0))[0]["met"] == false
        );
        assert!(g
            .event(&session(9, Activity::Pitching), &pitch(0.0, 39.0, 40.0))
            .is_empty());

        let out = g
            .message(serde_json::json!({"type": "result", "result": "Ground_Out"}))
            .unwrap();
        assert_eq!(out[0]["id"], "slap");
        assert!(g
            .message(serde_json::json!({"type": "result", "result": " "}))
            .is_err());
        assert!(g.message(serde_json::json!({"type": "nope"})).is_err());
        g.message(serde_json::json!({"type": "result", "result": "single", "runs": 1}))
            .unwrap();
        let out = g
            .message(serde_json::json!({"type": "result", "result": "strikeout"}))
            .unwrap();
        assert_eq!(out[0]["id"], "slugger", "the lineup wraps");
        let stored: Vec<(String, String, Option<u32>)> =
            g.db.prepare("SELECT batter, result, runs FROM challenges_results ORDER BY id")
                .unwrap()
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .map(Result::unwrap)
                .collect();
        assert_eq!(stored[0], ("slugger".into(), "ground_out".into(), None));
        assert_eq!(stored[1], ("slap".into(), "single".into(), Some(1)));

        let lines = g.session_ended(&session(2, Activity::Pitching));
        assert_eq!(lines[0].label, "GOALS MET");
        assert_eq!(lines[0].value, "1 OF 2");
        assert!(g.welcome().is_empty());
        assert!(g
            .message(serde_json::json!({"type": "result", "result": "single"}))
            .is_err());
    }

    #[test]
    fn contact_makes_a_batted_ball_shaped_like_a_hit() {
        let mut g = game();
        let slugger = g.config.batters[0].clone();
        let strike = pitch(0.0, 30.0, 40.0);
        let mut contacts = 0;
        for _ in 0..200 {
            if let Out::Swing {
                swing: SwingKind::Contact,
                record: Some(r),
            } = g.swing(&slugger, &strike)
            {
                contacts += 1;
                assert_eq!(r.kind, EventKind::Hit);
                assert_eq!(r.pitch_velocity, Some(40.0));
                assert!((20.0..=105.0).contains(&r.exit_velocity));
                assert!(r.hit_type.is_some());
                let hit = r.hit.expect("a predicted flight");
                assert!(hit.carry > 0.0 && hit.path.len() >= 2);
            }
        }
        assert!(contacts > 20, "{contacts} contacts in 200 swings");
    }
}
