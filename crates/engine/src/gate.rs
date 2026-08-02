//! The first-class gate interface (ticket
//! `.kranz/tickets/gate-plugin-interface.md`, KRZ-311; scored-gates addendum
//! KRZ-315).
//!
//! Until this module every gate in the engine was bespoke — contract command
//! assertions, scrutiny validators, the empty-deliverable final gate,
//! [`crate::merge_gate`], [`crate::workspace_gate`], preflight — and each
//! invented its own pass/fail shape, so nothing could register, order, or
//! record gates uniformly. This module is the shared contract: a [`Gate`] is
//! ordered, typed, independently registrable into a [`GatePipeline`], and
//! every evaluation returns a [`GateOutcome`] — an authoritative
//! [`GateVerdict`] plus an [`ArtefactRef`] handle to the evidence behind it.
//!
//! WHY the ordering is structural: deterministic gates (exit codes, lints,
//! scans) are cheap and reproducible, while model-judged gates spend tokens
//! and want the deterministic evidence in hand first. A [`GatePipeline`]
//! therefore cannot represent "model gate ahead of deterministic gate" at
//! all — it stores the two kinds in separate sections and evaluates every
//! deterministic gate (in registration order) before any model-judged one
//! (also in registration order). There is no insertion-position API to get
//! wrong, and registration can only ever choose an order WITHIN a section.
//!
//! WHY the score is optional and inert (KRZ-315): a gate may report a
//! confidence score and the threshold it judged against so a later slice
//! (`gate-confidence-score`) can persist and query low-confidence verdicts.
//! The score never decides anything here — [`GateOutcome`] has no
//! constructor that derives a verdict from a score, so the verdict a gate
//! states IS the verdict. Boolean-only gates simply never name the field.
//!
//! WHY artefacts are references, not events: persisting outcomes as
//! first-class events is `gate-results-first-class-events` (KRZ-312). This
//! module fixes only the handle shape so that slice needs no rework here.
//!
//! Ownership is unchanged by this interface: who WRITES a gate's config is a
//! property of the reader, not the registry. The merge-gate suite stays
//! base-branch-owned (its bytes are read from the live base sha in
//! [`crate::merge`]), so a mission cannot weaken or reorder the gates that
//! judge its own diff.

/// Whether a gate's verdict comes from a reproducible check or from model
/// judgement. The kind selects the gate's pipeline section — deterministic
/// gates always evaluate first (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateKind {
    /// Exit codes, lints, scans: cheap, reproducible, no model spend.
    Deterministic,
    /// A model session's judgement (scrutiny, review): spends tokens and may
    /// carry a confidence score.
    ModelJudged,
}

/// The authoritative outcome of one gate evaluation: pass or fail, as
/// stated by the gate — never derived from [`GateOutcome::score`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateVerdict {
    Pass,
    Fail,
}

/// A gate-supplied confidence score and the threshold the gate judged it
/// against (KRZ-315). Purely evidentiary: nothing in this module reads it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GateScore {
    /// The gate's confidence in its own verdict (conventionally 0.0..=1.0;
    /// the scale is the gate's to define).
    pub score: f64,
    /// The threshold the gate judged the score against — below it, a later
    /// slice routes the verdict to a human.
    pub threshold: f64,
}

/// A handle to the evidence behind a verdict.
///
/// This is the reference a later slice persists as a first-class event
/// (KRZ-312); it is deliberately just a handle plus optional captured
/// content, not an event itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtefactRef {
    /// Stable handle naming where the evidence lives: a repo-relative path,
    /// the command line that ran, a session id — whatever re-finds it.
    pub reference: String,
    /// Captured content worth keeping verbatim (a failing command's output,
    /// a validator's reply excerpt). `None` when the reference alone is the
    /// evidence.
    pub detail: Option<String>,
}

impl ArtefactRef {
    pub fn new(reference: impl Into<String>) -> Self {
        Self {
            reference: reference.into(),
            detail: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// What one gate evaluation produced: an authoritative verdict, the
/// artefact behind it, and an optional confidence score.
#[derive(Debug, Clone, PartialEq)]
pub struct GateOutcome {
    pub verdict: GateVerdict,
    pub artefact: ArtefactRef,
    /// Optional gate-supplied score (KRZ-315). `None` for boolean-only
    /// gates; never consulted to compute `verdict`.
    pub score: Option<GateScore>,
}

impl GateOutcome {
    /// A passing outcome. There is deliberately no constructor that takes a
    /// score and computes a verdict — the verdict is always stated.
    pub fn pass(artefact: ArtefactRef) -> Self {
        Self {
            verdict: GateVerdict::Pass,
            artefact,
            score: None,
        }
    }

    /// A failing outcome; see [`GateOutcome::pass`] on verdicts vs scores.
    pub fn fail(artefact: ArtefactRef) -> Self {
        Self {
            verdict: GateVerdict::Fail,
            artefact,
            score: None,
        }
    }

    /// Attach a confidence score + threshold without touching the verdict.
    pub fn with_score(mut self, score: f64, threshold: f64) -> Self {
        self.score = Some(GateScore { score, threshold });
        self
    }

    pub fn passed(&self) -> bool {
        self.verdict == GateVerdict::Pass
    }
}

/// A gate: an ordered, typed, independently registrable check.
///
/// Gates capture everything they need at construction (paths, suites,
/// executors, sessions) so registration is uniform; `evaluate` takes no
/// shared context because a shell-command gate and a scrutiny gate share no
/// honest input type.
pub trait Gate {
    /// Stable identity for reports and (later) persisted events.
    fn name(&self) -> &str;
    /// Selects the pipeline section; deterministic gates evaluate first.
    fn kind(&self) -> GateKind;
    /// Run the check and return its outcome.
    fn evaluate(&self) -> GateOutcome;
}

/// One gate's outcome, annotated with the identity and kind the pipeline
/// registered it under.
#[derive(Debug, Clone)]
pub struct GateReport {
    pub name: String,
    pub kind: GateKind,
    pub outcome: GateOutcome,
}

/// An ordered gate sequence with the deterministic/model-judged ordering
/// encoded in its storage: two sections, evaluated deterministic-first, so a
/// pipeline with a model gate ahead of a deterministic gate is
/// unrepresentable rather than merely rejected.
#[derive(Default)]
pub struct GatePipeline {
    deterministic: Vec<Box<dyn Gate>>,
    model_judged: Vec<Box<dyn Gate>>,
}

impl GatePipeline {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a gate. Its declared [`Gate::kind`] selects the section —
    /// registration chooses an order within a section, never a position
    /// across sections.
    pub fn register(&mut self, gate: Box<dyn Gate>) -> &mut Self {
        match gate.kind() {
            GateKind::Deterministic => self.deterministic.push(gate),
            GateKind::ModelJudged => self.model_judged.push(gate),
        }
        self
    }

    pub fn len(&self) -> usize {
        self.deterministic.len() + self.model_judged.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Evaluate every gate in pipeline order: all deterministic gates in
    /// registration order, then all model-judged gates in registration
    /// order. Every gate runs and every outcome is returned — stopping at
    /// the first failure (as the merge-gate suite does internally) is a
    /// gate's or caller's policy, not the registry's.
    pub fn evaluate(&self) -> Vec<GateReport> {
        self.deterministic
            .iter()
            .chain(self.model_judged.iter())
            .map(|gate| GateReport {
                name: gate.name().to_string(),
                kind: gate.kind(),
                outcome: gate.evaluate(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A scripted gate: records the order it was evaluated in and returns a
    /// fixed boolean outcome.
    struct ScriptedGate {
        name: &'static str,
        kind: GateKind,
        verdict: GateVerdict,
        calls: Rc<RefCell<Vec<&'static str>>>,
    }

    impl Gate for ScriptedGate {
        fn name(&self) -> &str {
            self.name
        }
        fn kind(&self) -> GateKind {
            self.kind
        }
        fn evaluate(&self) -> GateOutcome {
            self.calls.borrow_mut().push(self.name);
            match self.verdict {
                GateVerdict::Pass => GateOutcome::pass(ArtefactRef::new(self.name)),
                GateVerdict::Fail => GateOutcome::fail(ArtefactRef::new(self.name)),
            }
        }
    }

    fn scripted(
        name: &'static str,
        kind: GateKind,
        calls: &Rc<RefCell<Vec<&'static str>>>,
    ) -> Box<dyn Gate> {
        Box::new(ScriptedGate {
            name,
            kind,
            verdict: GateVerdict::Pass,
            calls: Rc::clone(calls),
        })
    }

    /// The ordering rule: registering model-judged gates FIRST must still
    /// evaluate them after every deterministic gate — the pipeline has no
    /// representation for "model ahead of deterministic".
    #[test]
    fn gate_plugin_model_gates_cannot_precede_deterministic_gates() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let mut pipeline = GatePipeline::new();
        pipeline
            .register(scripted("model-a", GateKind::ModelJudged, &calls))
            .register(scripted("det-a", GateKind::Deterministic, &calls))
            .register(scripted("model-b", GateKind::ModelJudged, &calls))
            .register(scripted("det-b", GateKind::Deterministic, &calls));
        assert_eq!(pipeline.len(), 4);

        let reports = pipeline.evaluate();

        let expected = ["det-a", "det-b", "model-a", "model-b"];
        assert_eq!(
            reports
                .iter()
                .map(|report| report.name.as_str())
                .collect::<Vec<_>>(),
            expected,
            "deterministic section first, registration order within each section"
        );
        assert_eq!(
            reports.iter().map(|report| report.kind).collect::<Vec<_>>(),
            [
                GateKind::Deterministic,
                GateKind::Deterministic,
                GateKind::ModelJudged,
                GateKind::ModelJudged,
            ]
        );
        assert_eq!(
            *calls.borrow(),
            expected,
            "evaluation ran in pipeline order"
        );
        assert!(reports.iter().all(|report| report.outcome.passed()));
    }

    /// A boolean-only gate: its `evaluate` never names score or threshold.
    struct BooleanGate;

    impl Gate for BooleanGate {
        fn name(&self) -> &str {
            "boolean-gate"
        }
        fn kind(&self) -> GateKind {
            GateKind::Deterministic
        }
        fn evaluate(&self) -> GateOutcome {
            GateOutcome::pass(ArtefactRef::new("lint.log"))
        }
    }

    #[test]
    fn gate_plugin_boolean_gate_runs_without_a_score() {
        let mut pipeline = GatePipeline::new();
        pipeline.register(Box::new(BooleanGate));
        let reports = pipeline.evaluate();
        assert_eq!(reports.len(), 1);
        assert!(reports[0].outcome.passed());
        assert_eq!(reports[0].outcome.score, None);
        assert_eq!(reports[0].outcome.artefact.reference, "lint.log");
    }

    #[test]
    fn gate_plugin_verdict_is_never_derived_from_the_score() {
        let confident_failure = GateOutcome::fail(ArtefactRef::new("review")).with_score(0.99, 0.5);
        assert!(
            !confident_failure.passed(),
            "a high score must not flip a stated Fail"
        );
        let nervous_pass = GateOutcome::pass(ArtefactRef::new("review")).with_score(0.1, 0.9);
        assert!(
            nervous_pass.passed(),
            "a low score must not flip a stated Pass"
        );
    }

    #[test]
    fn gate_plugin_scored_gate_carries_score_and_threshold() {
        let outcome = GateOutcome::pass(ArtefactRef::new("scrutiny")).with_score(0.42, 0.75);
        let score = outcome.score.expect("score recorded");
        assert_eq!(score.score, 0.42);
        assert_eq!(score.threshold, 0.75);
        assert!(outcome.passed());
    }
}
