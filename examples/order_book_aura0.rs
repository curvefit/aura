use aura_codec::{
    generic_i64_parent_schema, AuraI64EventReader, AuraI64EventWriter, I64Event, Profile,
};

fn main() -> Result<(), aura_codec::AuraError> {
    let schema =
        generic_i64_parent_schema("order-book-events-v1", &[100, 0, 200, 205, 0, 0, 5, 0])?;
    let event = I64Event {
        event_values: vec![1_000, 1],
        children: vec![vec![0, 100, 20, 19, 2], vec![1, 101, 30, 30, 3]],
    };
    let mut writer = AuraI64EventWriter::new(schema);
    writer.push_event(event.clone())?;
    let complete_aura0 = writer.finish_profile(Profile::Aura0)?;
    let reader = AuraI64EventReader::open(&complete_aura0)?;
    assert_eq!(reader.events(), &[event]);
    println!("complete_aura0_bytes={}", complete_aura0.len());
    Ok(())
}
