use aura_codec::generic_planner::I64SearchEffort;
use aura_codec::{
    generic_i64_parent_schema, AuraI64EventReader, AuraI64EventWriter, I64Event, Profile,
};

fn writer() -> AuraI64EventWriter {
    let schema =
        generic_i64_parent_schema("bounded-search-test", &[100, 0, 200, 205, 0, 0, 5, 0]).unwrap();
    let mut w = AuraI64EventWriter::new(schema).with_header_comment("retained metadata");
    for i in 0..150i64 {
        let children = if i % 7 == 0 {
            vec![]
        } else {
            vec![
                vec![0, 100 + i, 20, 19, 0],
                vec![1, 101 + i, 0, 0, 2],
                vec![0, i64::MAX, i64::MAX, i64::MIN, 3],
            ]
        };
        w.push_event(I64Event {
            event_values: vec![1000, i / 2],
            children,
        })
        .unwrap();
    }
    w
}

#[test]
fn bounded_preserves_boundaries_extremes_parent_fallback_and_metadata() {
    let expected = writer().events().to_vec();
    for effort in [I64SearchEffort::Full, I64SearchEffort::Bounded] {
        let bytes = writer().finish_aura0_with_search(effort).unwrap();
        let public = AuraI64EventReader::open(&bytes).unwrap();
        let independent = aura_codec::records::decode_i64_events_file(&bytes).unwrap();
        assert_eq!(public.events(), expected);
        assert_eq!(independent.events, expected);
        assert_eq!(public.header(), &independent.header);
        assert_eq!(public.schema(), &independent.schema);
    }
}

#[test]
fn default_bytes_and_concurrent_writer_choices_are_independent() {
    let expected = writer().finish_profile(Profile::Aura0).unwrap();
    let full = std::thread::spawn(|| {
        writer()
            .finish_aura0_with_search(I64SearchEffort::Full)
            .unwrap()
    });
    let bounded = std::thread::spawn(|| {
        writer()
            .finish_aura0_with_search(I64SearchEffort::Bounded)
            .unwrap()
    });
    assert_eq!(full.join().unwrap(), expected);
    assert_eq!(
        AuraI64EventReader::open(&bounded.join().unwrap())
            .unwrap()
            .events(),
        writer().events()
    );
    assert_eq!(writer().finish_profile(Profile::Aura0).unwrap(), expected);
}

#[test]
fn bounded_does_not_silently_change_other_profiles() {
    let result = aura_codec::records::encode_i64_events_profile_with_search(
        writer().into_input(),
        Profile::Ingest,
        I64SearchEffort::Bounded,
    );
    assert!(result.is_err());
}
