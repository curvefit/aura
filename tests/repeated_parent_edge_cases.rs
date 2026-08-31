use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};

use aura_codec::format::{AURA_MAGIC, SEAL_MAGIC};
use aura_codec::instructions::{DerivedOp, GenericGroupInstruction};
use aura_codec::records;
use aura_codec::{
    generic_i64_parent_schema, AuraI64EventReader, AuraI64EventWriter, I64Event, Profile,
};
use sha2::{Digest, Sha256};

const RELATED_SCHEMA_MAP: &[u8] = &[100, 0, 200, 204, 0, 0, 5];
const DIRECT_SCHEMA_MAP: &[u8] = &[100, 0, 200, 204, 0, 0, 0];

// This i64 schema has no nullable source fields; null coverage belongs in the
// grouped nullable tests.
fn edge_case_events() -> Vec<I64Event> {
    let mut events = vec![
        // Bid-only: includes a repeated price and a present zero residual
        // (QTY1 == QTY2, so the residual is zero).
        I64Event {
            event_values: vec![1_700_000_000_000_000_000, 11],
            children: vec![
                vec![0, 100_000, 1, 1],
                vec![0, 100_000, 2, 1],
                vec![0, 99_999, 3, 2],
            ],
        },
        // Ask-only with tiny legal quantities.
        I64Event {
            event_values: vec![1_700_000_000_000_000_001, 12],
            children: vec![vec![1, 100_001, 1, 1], vec![1, 100_002, 2, 1]],
        },
        // An authoritative empty event must remain an event in the complete
        // explicit-event representation.
        I64Event {
            event_values: vec![1_700_000_000_000_000_002, 13],
            children: Vec::new(),
        },
        // Alternating domains in source order and an explicit zero/zero
        // delete. The relationship residual is zero for this delete.
        I64Event {
            event_values: vec![1_700_000_000_000_000_003, 14],
            children: vec![
                vec![0, 100_010, 0, 0],
                vec![1, 100_010, 7, 3],
                vec![0, 100_011, 9, 9],
                vec![1, 100_011, 11, 8],
            ],
        },
        // Very large but legal i64 quantities, with a small residual.
        I64Event {
            event_values: vec![1_700_000_000_000_000_004, 15],
            children: vec![
                vec![0, 100_020, i64::MAX - 4_096, i64::MAX - 8_192],
                vec![1, 100_021, i64::MAX - 2_048, i64::MAX - 2_049],
            ],
        },
        // A later nonzero update replaces the earlier delete for the same
        // (side, price) key in the independent replay oracle below.
        I64Event {
            event_values: vec![1_700_000_000_000_000_005, 16],
            children: vec![vec![0, 100_010, 13, 13]],
        },
    ];

    // Keep the residual lane low-entropy while making cardinality, side
    // ordering, repeated prices, and event values vary enough to exercise the
    // complete event planner rather than a one-row special case.
    for event_index in 0..160i64 {
        let child_count = match event_index % 9 {
            0 => 1,
            1 => 2,
            2 => 5,
            3 => 8,
            4 => 13,
            5 => 21,
            6 => 34,
            7 => 55,
            _ => 64,
        };
        let children = (0..child_count)
            .map(|child_index| {
                let side = if event_index % 2 == 0 {
                    child_index & 1
                } else {
                    1 - (child_index & 1)
                };
                let price = 200_000 + (child_index / 2) * 3 + (event_index % 17);
                let total = if event_index % 37 == 0 && child_index == 0 {
                    0
                } else if child_index % 19 == 0 {
                    1 + (event_index % 3)
                } else {
                    1_000_000_000_000 + ((event_index * 64 + child_index) % 10_000)
                };
                let residual = (event_index + child_index).rem_euclid(5);
                let qty2 = total - residual;
                vec![side, price, total, qty2]
            })
            .collect();
        events.push(I64Event {
            event_values: vec![1_700_000_000_000_001_000 + event_index, 1_000 + event_index],
            children,
        });
    }
    events
}

fn seal_events(schema_map: &[u8], events: &[I64Event]) -> Vec<u8> {
    let schema = generic_i64_parent_schema("repeated-parent-edge-cases", schema_map).unwrap();
    let mut writer = AuraI64EventWriter::new(schema);
    for event in events {
        writer.push_event(event.clone()).unwrap();
    }
    writer.finish_profile(Profile::Aura0).unwrap()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReplaySummary {
    event_count: usize,
    child_count: usize,
    final_state_hash: [u8; 32],
}

fn replay_summary(events: &[I64Event]) -> ReplaySummary {
    let mut state = BTreeMap::<(i64, i64), i64>::new();
    let mut child_count = 0;
    for event in events {
        child_count += event.children.len();
        for child in &event.children {
            assert_eq!(4, child.len(), "replay fixture child width");
            let key = (child[0], child[1]);
            let total = child[2];
            if total == 0 {
                state.remove(&key);
            } else {
                // BTreeMap::insert intentionally replaces an earlier value
                // when the same domain/price occurs again later in a stream.
                state.insert(key, total);
            }
        }
    }

    let mut hasher = Sha256::new();
    hasher.update(b"aura-repeated-parent-final-state-v1\0");
    hasher.update((state.len() as u64).to_le_bytes());
    for ((side, price), total) in state {
        hasher.update(side.to_le_bytes());
        hasher.update(price.to_le_bytes());
        hasher.update(total.to_le_bytes());
    }
    ReplaySummary {
        event_count: events.len(),
        child_count,
        final_state_hash: hasher.finalize().into(),
    }
}

fn assert_replay_summary(label: &str, expected: &ReplaySummary, events: &[I64Event]) {
    let actual = replay_summary(events);
    assert_eq!(
        expected.event_count, actual.event_count,
        "{label} event count"
    );
    assert_eq!(
        expected.child_count, actual.child_count,
        "{label} child count"
    );
    assert_eq!(
        expected.final_state_hash, actual.final_state_hash,
        "{label} final state hash"
    );
}

fn assert_rejected_without_panic(label: &str, bytes: &[u8]) {
    let result = catch_unwind(AssertUnwindSafe(|| records::decode_i64_events_file(bytes)));
    match result {
        Ok(Err(_)) => {}
        Ok(Ok(_)) => panic!("{label} unexpectedly decoded successfully"),
        Err(_) => panic!("{label} panicked during decode"),
    }
}

#[test]
fn repeated_parent_complete_aura0_covers_edge_cases_and_aura1() {
    let events = edge_case_events();
    let source_summary = replay_summary(&events);
    let related_aura0 = seal_events(RELATED_SCHEMA_MAP, &events);

    // A complete V2 Aura0 file has the ordinary header and sealed trailer;
    // opening it through the normal event reader also validates its footer.
    assert_eq!(AURA_MAGIC, &related_aura0[..AURA_MAGIC.len()]);
    assert!(related_aura0.ends_with(SEAL_MAGIC));
    let normal_reader = AuraI64EventReader::open(&related_aura0).unwrap();
    assert_eq!(Profile::Aura0, normal_reader.header().profile);
    assert_eq!(&events, normal_reader.events());
    assert_replay_summary(
        "related Aura0 event reader",
        &source_summary,
        normal_reader.events(),
    );

    let decoded = records::decode_i64_events_file(&related_aura0).unwrap();
    assert_eq!(events, decoded.events);
    assert_replay_summary(
        "related Aura0 records decoder",
        &source_summary,
        &decoded.events,
    );
    let footer = decoded.compiled_footer.as_ref().unwrap();
    let plan = footer.generic_aura0_plan.as_ref().unwrap();
    assert!(
        plan.groups.iter().any(|group| matches!(
            group,
            GenericGroupInstruction::DerivedStream {
                parent_group_id: None,
                output_slot: 5,
                op: DerivedOp::AddResidual | DerivedOp::SubtractResidual,
                input_slots,
                ..
            } if input_slots == &[4]
        )),
        "compiled plan did not select slot 5 residual from slot 4: {plan:#?}"
    );

    let direct_aura0 = seal_events(DIRECT_SCHEMA_MAP, &events);
    assert!(
        direct_aura0.len() > related_aura0.len(),
        "direct schema unexpectedly won: direct={} related={}",
        direct_aura0.len(),
        related_aura0.len()
    );
    assert_eq!(
        events,
        records::decode_i64_events_file(&direct_aura0)
            .unwrap()
            .events
    );
    let direct_decoded = records::decode_i64_events_file(&direct_aura0).unwrap();
    assert_replay_summary(
        "direct Aura0 records decoder",
        &source_summary,
        &direct_decoded.events,
    );

    let aura1 = records::compile_i64_file(&related_aura0, Profile::Aura1).unwrap();
    let aura1_reader = AuraI64EventReader::open(&aura1).unwrap();
    assert_eq!(events, aura1_reader.events());
    assert_replay_summary("Aura1 event reader", &source_summary, aura1_reader.events());
    let aura1_decoded = records::decode_i64_events_file(&aura1).unwrap();
    assert_eq!(events, aura1_decoded.events);
    assert_replay_summary(
        "Aura1 records decoder",
        &source_summary,
        &aura1_decoded.events,
    );

    let metadata = records::decode_i64_file_metadata(&related_aura0).unwrap();
    assert!(
        metadata.footer_start > metadata.header_len,
        "Aura0 body is empty"
    );
    let mut truncation = related_aura0.clone();
    truncation.pop();
    assert_rejected_without_panic("truncated Aura0", &truncation);

    let mut bad_seal = related_aura0.clone();
    let last = bad_seal.len() - 1;
    bad_seal[last] ^= 1;
    assert_rejected_without_panic("bad Aura0 seal", &bad_seal);

    let mut trailing_byte = related_aura0.clone();
    trailing_byte.push(0xa5);
    assert_rejected_without_panic("Aura0 trailing byte", &trailing_byte);

    let mut body_corruption = related_aura0.clone();
    body_corruption[metadata.header_len] ^= 0xff;
    assert_rejected_without_panic("Aura0 body byte corruption", &body_corruption);
}
