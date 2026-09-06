use kranz_engine::{
    digest,
    events::{Event, EventKind},
    reducer,
    types::MissionConfig,
};

fn main() {
    let created = Event {
        seq: 1,
        ts: Default::default(),
        mission_id: "m-budget-probe".into(),
        kind: EventKind::MissionCreated {
            goal: "Synthetic budget visibility probe".into(),
            base_branch: "main".into(),
            mission_branch: "kranz/mission-m-budget-probe".into(),
            config: MissionConfig::default(),
        },
    };
    let mut state = reducer::fold(&[created]).unwrap();
    assert_eq!(state.config.max_fix_cycles_per_milestone, 2);
    let before = digest::render(&state);
    let reseed_before = digest::render_reseed(&state, "{}");
    reducer::apply(
        &mut state,
        &Event {
            seq: 2,
            ts: Default::default(),
            mission_id: "m-budget-probe".into(),
            kind: EventKind::ConfigChanged {
                patch: r#"{"maxFixCyclesPerMilestone":3}"#.parse().unwrap(),
            },
        },
    )
    .unwrap();
    assert_eq!(state.config.max_fix_cycles_per_milestone, 3);
    assert_eq!(state.last_seq, 2);
    assert_eq!(before, digest::render(&state));
    assert_eq!(reseed_before, digest::render_reseed(&state, "{}"));
    println!("CONFIRMED: accepted cap 2 -> 3; digest and reseed byte-identical");
    reducer::apply(
        &mut state,
        &Event {
            seq: 3,
            ts: Default::default(),
            mission_id: "m-budget-probe".into(),
            kind: EventKind::ConfigChanged {
                patch: r#"{"maxFixCyclesPerMilestone":1}"#.parse().unwrap(),
            },
        },
    )
    .unwrap();
    assert_eq!(state.config.max_fix_cycles_per_milestone, 1);
    assert_eq!(before, digest::render(&state));
    println!("CONFIRMED: accepted cap 3 -> 1; digest remains byte-identical");
}
