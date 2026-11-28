//! Temperature alarms: rules and the state machine that evaluates them on live frames.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::frame::{FrameStats, ThermalFrame};
use super::measure::Spots;
use super::shapes::Shapes;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Metric {
    Max,
    Min,
    Mean,
}

impl Metric {
    pub const ALL: [Metric; 3] = [Metric::Max, Metric::Min, Metric::Mean];

    pub fn label(self) -> &'static str {
        match self {
            Metric::Max => "최고",
            Metric::Min => "최저",
            Metric::Mean => "평균",
        }
    }
}

/// What a rule watches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Source {
    Frame(Metric),
    Center,
    Spot(u32),
    Shape(u32, Metric),
}

impl Source {
    pub fn label(self, shapes: &Shapes) -> String {
        match self {
            Source::Frame(m) => format!("화면 {}", m.label()),
            Source::Center => "중앙".into(),
            Source::Spot(id) => format!("S{id}"),
            Source::Shape(id, m) => {
                let name = shapes
                    .items
                    .iter()
                    .find(|s| s.id == id)
                    .map(|s| s.label())
                    .unwrap_or_else(|| format!("#{id}"));
                format!("{name} {}", m.label())
            }
        }
    }

    /// Current value, or `None` if the measurement is gone or outside the frame.
    pub fn read(
        self,
        f: &ThermalFrame,
        st: &FrameStats,
        spots: &Spots,
        shapes: &Shapes,
    ) -> Option<f32> {
        let v = match self {
            Source::Frame(Metric::Max) => f.value(st.max),
            Source::Frame(Metric::Min) => f.value(st.min),
            Source::Frame(Metric::Mean) => f.value_f(st.mean),
            Source::Center => f.value_around(f.width / 2, f.height / 2, 1),
            Source::Spot(id) => spots.items.iter().find(|s| s.id == id)?.value(f)?,
            Source::Shape(id, m) => {
                let s = shapes.items.iter().find(|s| s.id == id)?.stats(f)?;
                match m {
                    Metric::Max => s.max,
                    Metric::Min => s.min,
                    Metric::Mean => s.mean,
                }
            }
        };
        v.is_finite().then_some(v)
    }

    /// False once the Spot, Box or Line it reads has been deleted.
    pub fn exists(self, spots: &Spots, shapes: &Shapes) -> bool {
        match self {
            Source::Frame(_) | Source::Center => true,
            Source::Spot(id) => spots.items.iter().any(|s| s.id == id),
            Source::Shape(id, _) => shapes.items.iter().any(|s| s.id == id),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Condition {
    Above,
    Below,
}

impl Condition {
    pub fn label(self) -> &'static str {
        match self {
            Condition::Above => "이상",
            Condition::Below => "이하",
        }
    }

    fn tripped(self, v: f32, threshold: f32) -> bool {
        match self {
            Condition::Above => v >= threshold,
            Condition::Below => v <= threshold,
        }
    }

    fn released(self, v: f32, threshold: f32, hysteresis: f32) -> bool {
        match self {
            Condition::Above => v <= threshold - hysteresis,
            Condition::Below => v >= threshold + hysteresis,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub id: u32,
    pub enabled: bool,
    pub source: Source,
    pub condition: Condition,
    /// °C.
    pub threshold: f32,
}

impl Rule {
    pub fn describe(&self, shapes: &Shapes) -> String {
        format!(
            "{} {:.1} °C {}",
            self.source.label(shapes),
            self.threshold,
            self.condition.label()
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AlarmConfig {
    pub enabled: bool,
    pub rules: Vec<Rule>,
    /// How long the condition must hold before the alarm is raised.
    pub hold_s: f32,
    /// How far back past the threshold the value must return to clear the alarm (°C).
    pub hysteresis: f32,
    /// Save a snapshot when an alarm is raised.
    pub snapshot_on_raise: bool,
    next_id: u32,
}

impl Default for AlarmConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            rules: Vec::new(),
            hold_s: 1.0,
            hysteresis: 0.5,
            snapshot_on_raise: false,
            next_id: 0,
        }
    }
}

impl AlarmConfig {
    pub const MAX_RULES: usize = 8;

    pub fn add(&mut self, source: Source, condition: Condition, threshold: f32) -> Option<u32> {
        if self.rules.len() >= Self::MAX_RULES {
            return None;
        }
        self.next_id += 1;
        self.rules.push(Rule {
            id: self.next_id,
            enabled: true,
            source,
            condition,
            threshold,
        });
        Some(self.next_id)
    }

    pub fn remove(&mut self, id: u32) {
        self.rules.retain(|r| r.id != id);
    }

    /// Drop rules whose measurement was deleted. Measurement ids can be reused, so a stale rule
    /// could otherwise start watching a different spot. Returns how many were removed.
    pub fn prune(&mut self, spots: &Spots, shapes: &Shapes) -> usize {
        let before = self.rules.len();
        self.rules.retain(|r| r.source.exists(spots, shapes));
        before - self.rules.len()
    }

    /// True if at least one rule would be evaluated.
    pub fn is_armed(&self) -> bool {
        self.enabled && self.rules.iter().any(|r| r.enabled)
    }
}

/// Why rules are not being evaluated right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Suspended {
    Playback,
    /// The image is held (frozen, or a saved snapshot is open): no new frames to judge.
    Frozen,
    NoSignal,
    NotRadiometric,
}

impl Suspended {
    pub fn label(self) -> &'static str {
        match self {
            Suspended::Playback => "녹화 재생 중",
            Suspended::Frozen => "화면 정지 중",
            Suspended::NoSignal => "카메라 영상 없음",
            Suspended::NotRadiometric => "온도값이 아닌 원시값 수신 중",
        }
    }
}

/// A rule changing between normal and alarmed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transition {
    pub rule_id: u32,
    pub raised: bool,
    pub value: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    Clear,
    Pending { since: f64 },
    Active,
}

#[derive(Debug, Clone, Copy)]
struct RuleState {
    /// Editing what the rule measures restarts its evaluation.
    key: (Source, Condition, u32),
    phase: Phase,
    value: f32,
}

#[derive(Debug)]
pub struct Monitor {
    states: HashMap<u32, RuleState>,
    suspended: Option<Suspended>,
    unacknowledged: u32,
}

impl Default for Monitor {
    fn default() -> Self {
        Self {
            states: HashMap::new(),
            suspended: Some(Suspended::NoSignal),
            unacknowledged: 0,
        }
    }
}

impl Monitor {
    pub fn suspended(&self) -> Option<Suspended> {
        self.suspended
    }

    /// Stop or resume evaluation. Stopping forgets every rule's state: without data, an alarm
    /// can be neither confirmed nor cleared, and the hold time must start over afterwards.
    pub fn set_suspended(&mut self, why: Option<Suspended>) {
        if why.is_some() {
            self.states.clear();
        }
        self.suspended = why;
    }

    /// Evaluate all rules on a new frame taken at `t` seconds.
    pub fn update(
        &mut self,
        cfg: &AlarmConfig,
        t: f64,
        mut read: impl FnMut(Source) -> Option<f32>,
    ) -> Vec<Transition> {
        if self.suspended.is_some() || !cfg.enabled {
            self.states.clear();
            return Vec::new();
        }
        self.states
            .retain(|id, _| cfg.rules.iter().any(|r| r.id == *id && r.enabled));

        let hold = cfg.hold_s.max(0.0) as f64;
        let hysteresis = cfg.hysteresis.max(0.0);
        let mut out = Vec::new();
        for r in cfg.rules.iter().filter(|r| r.enabled) {
            let Some(v) = read(r.source) else {
                self.states.remove(&r.id);
                continue;
            };
            let key = (r.source, r.condition, r.threshold.to_bits());
            let state = self.states.entry(r.id).or_insert(RuleState {
                key,
                phase: Phase::Clear,
                value: v,
            });
            if state.key != key {
                state.key = key;
                state.phase = Phase::Clear;
            }
            state.value = v;

            let tripped = r.condition.tripped(v, r.threshold);
            match state.phase {
                Phase::Active => {
                    if r.condition.released(v, r.threshold, hysteresis) {
                        state.phase = Phase::Clear;
                        out.push(Transition {
                            rule_id: r.id,
                            raised: false,
                            value: v,
                        });
                    }
                }
                Phase::Clear | Phase::Pending { .. } if !tripped => state.phase = Phase::Clear,
                Phase::Clear | Phase::Pending { .. } => {
                    let since = match state.phase {
                        Phase::Pending { since } => since,
                        _ => t,
                    };
                    if t - since >= hold {
                        state.phase = Phase::Active;
                        self.unacknowledged += 1;
                        out.push(Transition {
                            rule_id: r.id,
                            raised: true,
                            value: v,
                        });
                    } else {
                        state.phase = Phase::Pending { since };
                    }
                }
            }
        }
        out
    }

    pub fn is_active(&self, id: u32) -> bool {
        self.states
            .get(&id)
            .is_some_and(|s| s.phase == Phase::Active)
    }

    /// The condition is met and the hold time is running.
    pub fn is_pending(&self, id: u32) -> bool {
        self.states
            .get(&id)
            .is_some_and(|s| matches!(s.phase, Phase::Pending { .. }))
    }

    /// Latest value seen by a rule while it is being evaluated.
    pub fn value(&self, id: u32) -> Option<f32> {
        self.states.get(&id).map(|s| s.value)
    }

    /// Alarms raised since the user last acknowledged them, including ones that have cleared.
    pub fn unacknowledged(&self) -> u32 {
        self.unacknowledged
    }

    pub fn acknowledge(&mut self) {
        self.unacknowledged = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: Source = Source::Frame(Metric::Max);

    fn config(condition: Condition, threshold: f32) -> (AlarmConfig, u32) {
        let mut c = AlarmConfig::default();
        let id = c.add(SRC, condition, threshold).unwrap();
        (c, id)
    }

    fn running() -> Monitor {
        let mut m = Monitor::default();
        m.set_suspended(None);
        m
    }

    fn step(m: &mut Monitor, c: &AlarmConfig, t: f64, v: f32) -> Vec<Transition> {
        m.update(c, t, |_| Some(v))
    }

    #[test]
    fn raises_only_after_the_hold_time() {
        let (c, id) = config(Condition::Above, 50.0);
        let mut m = running();
        assert!(step(&mut m, &c, 0.0, 51.0).is_empty());
        assert!(step(&mut m, &c, 0.9, 51.0).is_empty());
        assert!(!m.is_active(id));
        let tr = step(&mut m, &c, 1.0, 52.0);
        assert_eq!(
            tr,
            vec![Transition {
                rule_id: id,
                raised: true,
                value: 52.0
            }]
        );
        assert!(m.is_active(id));
        assert_eq!(m.unacknowledged(), 1);
    }

    #[test]
    fn a_brief_spike_does_not_raise() {
        let (c, id) = config(Condition::Above, 50.0);
        let mut m = running();
        step(&mut m, &c, 0.0, 60.0);
        step(&mut m, &c, 0.5, 40.0);
        // The hold time restarts here.
        step(&mut m, &c, 0.6, 60.0);
        assert!(step(&mut m, &c, 1.5, 60.0).is_empty());
        assert!(!m.is_active(id));
        assert_eq!(step(&mut m, &c, 1.6, 60.0).len(), 1);
    }

    #[test]
    fn clears_past_the_hysteresis_and_does_not_chatter() {
        let (c, id) = config(Condition::Above, 50.0);
        let mut m = running();
        step(&mut m, &c, 0.0, 51.0);
        step(&mut m, &c, 1.0, 51.0);
        // Below the threshold but inside the 0.5 °C band: still alarmed, no new event.
        assert!(step(&mut m, &c, 2.0, 49.8).is_empty());
        assert!(step(&mut m, &c, 3.0, 50.2).is_empty());
        assert!(m.is_active(id));
        let tr = step(&mut m, &c, 4.0, 49.5);
        assert_eq!(tr.len(), 1);
        assert!(!tr[0].raised);
        assert!(!m.is_active(id));
        // A cleared alarm stays unacknowledged until the user confirms it.
        assert_eq!(m.unacknowledged(), 1);
        m.acknowledge();
        assert_eq!(m.unacknowledged(), 0);
    }

    #[test]
    fn below_condition_mirrors_above() {
        let (c, id) = config(Condition::Below, 10.0);
        let mut m = running();
        step(&mut m, &c, 0.0, 9.0);
        assert_eq!(step(&mut m, &c, 1.0, 9.0).len(), 1);
        assert!(step(&mut m, &c, 2.0, 10.4).is_empty());
        assert!(m.is_active(id));
        assert_eq!(step(&mut m, &c, 3.0, 10.5).len(), 1);
        assert!(!m.is_active(id));
    }

    #[test]
    fn suspending_forgets_state_and_restarts_the_hold() {
        let (c, id) = config(Condition::Above, 50.0);
        let mut m = running();
        step(&mut m, &c, 0.0, 60.0);
        step(&mut m, &c, 1.0, 60.0);
        assert!(m.is_active(id));
        m.set_suspended(Some(Suspended::NoSignal));
        assert!(!m.is_active(id));
        assert!(step(&mut m, &c, 2.0, 60.0).is_empty());
        m.set_suspended(None);
        assert!(step(&mut m, &c, 3.0, 60.0).is_empty());
        assert_eq!(step(&mut m, &c, 4.0, 60.0).len(), 1);
    }

    #[test]
    fn starts_suspended() {
        let (c, id) = config(Condition::Above, 50.0);
        let mut m = Monitor::default();
        assert_eq!(m.suspended(), Some(Suspended::NoSignal));
        step(&mut m, &c, 0.0, 60.0);
        assert!(step(&mut m, &c, 5.0, 60.0).is_empty());
        assert!(!m.is_active(id));
    }

    #[test]
    fn editing_a_rule_restarts_its_evaluation() {
        let (mut c, id) = config(Condition::Above, 50.0);
        let mut m = running();
        step(&mut m, &c, 0.0, 60.0);
        step(&mut m, &c, 1.0, 60.0);
        assert!(m.is_active(id));
        c.rules[0].threshold = 55.0;
        assert!(step(&mut m, &c, 1.1, 60.0).is_empty());
        assert!(!m.is_active(id));
        assert_eq!(step(&mut m, &c, 2.1, 60.0).len(), 1);
    }

    #[test]
    fn disabled_rules_and_config_are_not_evaluated() {
        let (mut c, id) = config(Condition::Above, 50.0);
        let mut m = running();
        step(&mut m, &c, 0.0, 60.0);
        step(&mut m, &c, 1.0, 60.0);
        c.rules[0].enabled = false;
        assert!(step(&mut m, &c, 2.0, 60.0).is_empty());
        assert!(!m.is_active(id));
        c.rules[0].enabled = true;
        c.enabled = false;
        step(&mut m, &c, 3.0, 60.0);
        assert!(step(&mut m, &c, 5.0, 60.0).is_empty());
        assert!(!m.is_active(id));
        assert!(!c.is_armed());
    }

    #[test]
    fn missing_value_drops_the_rule_state() {
        let (c, id) = config(Condition::Above, 50.0);
        let mut m = running();
        step(&mut m, &c, 0.0, 60.0);
        step(&mut m, &c, 1.0, 60.0);
        assert!(m.update(&c, 2.0, |_| None).is_empty());
        assert!(!m.is_active(id));
        assert_eq!(m.value(id), None);
    }

    #[test]
    fn prune_removes_rules_of_deleted_measurements() {
        let mut spots = Spots::default();
        let mut shapes = Shapes::default();
        let s = spots.add(5, 5).unwrap();
        let b = shapes
            .add(crate::thermal::shapes::ShapeKind::Box, (1, 1), (4, 4))
            .unwrap();
        let mut c = AlarmConfig::default();
        c.add(SRC, Condition::Above, 50.0);
        c.add(Source::Spot(s), Condition::Above, 50.0);
        c.add(Source::Shape(b, Metric::Mean), Condition::Below, 5.0);
        assert_eq!(c.prune(&spots, &shapes), 0);
        spots.remove(s);
        assert_eq!(c.prune(&spots, &shapes), 1);
        shapes.clear();
        assert_eq!(c.prune(&spots, &shapes), 1);
        assert_eq!(c.rules.len(), 1);
        assert_eq!(c.rules[0].source, SRC);
    }

    #[test]
    fn rule_count_is_limited() {
        let mut c = AlarmConfig::default();
        for _ in 0..AlarmConfig::MAX_RULES {
            assert!(c.add(SRC, Condition::Above, 1.0).is_some());
        }
        assert_eq!(c.add(SRC, Condition::Above, 1.0), None);
    }

    #[test]
    fn sources_read_the_right_values() {
        // 3×3 frame, 0..8 °C.
        let f = ThermalFrame::new(3, 3, (0..9).map(|v| 27315 + v * 100).collect(), Some(0.01));
        let st = f.stats();
        let mut spots = Spots::default();
        let mut shapes = Shapes::default();
        let s = spots.add(1, 1).unwrap();
        let b = shapes
            .add(crate::thermal::shapes::ShapeKind::Box, (0, 0), (1, 0))
            .unwrap();
        let read = |src: Source| src.read(&f, &st, &spots, &shapes).unwrap();
        assert!((read(Source::Frame(Metric::Max)) - 8.0).abs() < 1e-2);
        assert!((read(Source::Frame(Metric::Min)) - 0.0).abs() < 1e-2);
        assert!((read(Source::Frame(Metric::Mean)) - 4.0).abs() < 1e-2);
        assert!((read(Source::Center) - 4.0).abs() < 1e-2);
        assert!((read(Source::Spot(s)) - 4.0).abs() < 1e-2);
        assert!((read(Source::Shape(b, Metric::Max)) - 1.0).abs() < 1e-2);
        assert!((read(Source::Shape(b, Metric::Mean)) - 0.5).abs() < 1e-2);
        assert_eq!(Source::Spot(99).read(&f, &st, &spots, &shapes), None);
    }

    #[test]
    fn config_survives_json() {
        let (c, _) = config(Condition::Below, 12.5);
        let back: AlarmConfig = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back.rules, c.rules);
        let partial: AlarmConfig = serde_json::from_str("{}").unwrap();
        assert!(partial.enabled);
        assert_eq!(partial.hold_s, 1.0);
    }
}
