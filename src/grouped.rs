use crate::types::BookEvent;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupPolicy {
    pub max_group_size: u8,
}

impl Default for GroupPolicy {
    fn default() -> Self {
        Self { max_group_size: 4 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventGroup {
    pub start: usize,
    pub len: u8,
    pub ts_event: u64,
}

/// Plan same-timestamp groups for an Aura1 fixed-block layout.
///
/// Group sizes are powers of two so readers can specialize 1/2/4/8 paths.
pub fn plan_groups(events: &[BookEvent], policy: GroupPolicy) -> Vec<EventGroup> {
    let cap = largest_power_of_two(policy.max_group_size.max(1)).min(8);
    let mut groups = Vec::new();
    let mut index = 0usize;
    while index < events.len() {
        let ts = events[index].ts_event;
        let mut run_len = 1usize;
        while index + run_len < events.len() && events[index + run_len].ts_event == ts {
            run_len += 1;
        }
        let mut remaining = run_len;
        while remaining > 0 {
            let group_len = largest_power_of_two(remaining.min(usize::from(cap)) as u8);
            groups.push(EventGroup {
                start: index + (run_len - remaining),
                len: group_len,
                ts_event: ts,
            });
            remaining -= usize::from(group_len);
        }
        index += run_len;
    }
    groups
}

pub const fn largest_power_of_two(value: u8) -> u8 {
    if value >= 8 {
        8
    } else if value >= 4 {
        4
    } else if value >= 2 {
        2
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BookEvent, BookId};

    fn event(ts: u64, seq: u64) -> BookEvent {
        BookEvent::new(ts, seq, BookId::BookA, vec![], vec![])
    }

    #[test]
    fn planner_groups_repeated_timestamps() {
        let events = vec![
            event(1, 1),
            event(1, 2),
            event(1, 3),
            event(1, 4),
            event(1, 5),
            event(2, 6),
            event(3, 7),
            event(3, 8),
        ];

        let groups = plan_groups(&events, GroupPolicy { max_group_size: 4 });

        assert_eq!(
            vec![
                EventGroup {
                    start: 0,
                    len: 4,
                    ts_event: 1
                },
                EventGroup {
                    start: 4,
                    len: 1,
                    ts_event: 1
                },
                EventGroup {
                    start: 5,
                    len: 1,
                    ts_event: 2
                },
                EventGroup {
                    start: 6,
                    len: 2,
                    ts_event: 3
                },
            ],
            groups
        );
    }

    #[test]
    fn planner_caps_group_size() {
        let events: Vec<_> = (0..10).map(|idx| event(1, idx)).collect();

        let groups = plan_groups(&events, GroupPolicy { max_group_size: 8 });

        assert_eq!(
            vec![8, 2],
            groups.iter().map(|group| group.len).collect::<Vec<_>>()
        );
    }
}
