use aura_codec::{
    generic_i64_parent_schema, AuraI64EventReader, AuraI64EventWriter, I64Event, Profile,
};

const SCHEMA_MAP: &[u8] = &[100, 0, 200, 205, 0, 0, 5, 0];

fn synthetic_events() -> Vec<I64Event> {
    vec![
        I64Event {
            event_values: vec![1_000, 1],
            children: vec![vec![0, 100, 20, 19, 2], vec![1, 101, 30, 30, 3]],
        },
        I64Event {
            event_values: vec![1_001, 2],
            children: vec![],
        },
        I64Event {
            event_values: vec![1_002, 3],
            children: vec![vec![0, 99, 40, 37, 1]],
        },
    ]
}

fn writer(events: &[I64Event]) -> AuraI64EventWriter {
    let schema = generic_i64_parent_schema("synthetic-qty2-v1", SCHEMA_MAP).unwrap();
    let mut writer = AuraI64EventWriter::new(schema);
    for event in events {
        writer.push_event(event.clone()).unwrap();
    }
    writer
}

#[test]
fn direct_profile_matches_two_step_compilation_and_round_trips() {
    let events = synthetic_events();
    let direct = writer(&events).finish_profile(Profile::Aura0).unwrap();
    let ingest = writer(&events).finish().unwrap();
    let two_step = AuraI64EventWriter::compile_profile(&ingest, Profile::Aura0).unwrap();

    assert_eq!(direct, two_step);
    assert_eq!(AuraI64EventReader::open(&direct).unwrap().events(), events);
}
