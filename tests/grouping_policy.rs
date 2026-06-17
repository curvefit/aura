use aura_codec::legacy::grouped::{plan_groups, GroupPolicy};
use aura_codec::legacy::{BookEvent, BookId};

fn event(ts: u64, sequence: u64) -> BookEvent {
    BookEvent::new(ts, sequence, BookId::BookA, vec![], vec![])
}

#[test]
fn grouping_uses_power_of_two_runs_and_singletons() {
    let events = vec![
        event(10, 1),
        event(10, 2),
        event(10, 3),
        event(10, 4),
        event(10, 5),
        event(20, 6),
        event(30, 7),
        event(30, 8),
    ];

    let groups = plan_groups(&events, GroupPolicy { max_group_size: 4 });
    let lengths: Vec<_> = groups.iter().map(|group| group.len).collect();
    let starts: Vec<_> = groups.iter().map(|group| group.start).collect();

    assert_eq!(vec![4, 1, 1, 2], lengths);
    assert_eq!(vec![0, 4, 5, 6], starts);
}
