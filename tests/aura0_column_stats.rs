use aura_codec::stats::{signed_bit_width_for_range, zigzag_varint_len, FieldStats, PhysicalWidth};

#[test]
fn column_stats_detect_midpoint_base_delta_and_bit_widths() {
    let mut stats = FieldStats::new(0);
    for value in [990, 1_000, 1_010] {
        stats.observe(value);
    }

    assert_eq!(1_000, stats.midpoint_value());
    assert_eq!(10, stats.max_abs_midpoint_delta());
    assert_eq!(PhysicalWidth::I8, stats.midpoint_delta_width());
    assert_eq!(5, signed_bit_width_for_range(0, 31));
    assert_eq!(5, signed_bit_width_for_range(-10, 10));
}

#[test]
fn column_stats_detect_delta_of_deltas() {
    let mut stats = FieldStats::new(0);
    for value in [10, 20, 31, 43] {
        stats.observe(value);
    }

    assert_eq!(Some(10), stats.first_delta);
    assert_eq!(10, stats.min_delta);
    assert_eq!(12, stats.max_delta);
    assert_eq!(1, stats.min_delta2);
    assert_eq!(1, stats.max_delta2);
    assert_eq!(1, stats.max_abs_delta2);
    assert_eq!(PhysicalWidth::I8, stats.delta2_width());
}

#[test]
fn column_stats_detect_rough_steps_and_gaps() {
    let mut rough = FieldStats::new(0);
    for value in [100, 204, 313] {
        rough.observe(value);
    }
    let rough_step = rough.rough_step.unwrap();

    assert_eq!(104, rough_step.step);
    assert_eq!(5, rough_step.max_abs_residual);
    assert_eq!(0, rough_step.gap_count);

    let mut gaps = FieldStats::new(0);
    for value in [100, 200, 400, 500] {
        gaps.observe(value);
    }
    let gap_step = gaps.rough_step.unwrap();

    assert_eq!(100, gap_step.step);
    assert_eq!(2, gap_step.max_gap_steps);
    assert_eq!(1, gap_step.gap_count);
    assert_eq!(0, gap_step.max_abs_residual);
}

#[test]
fn column_stats_estimate_zigzag_varint_delta_costs() {
    assert_eq!(1, zigzag_varint_len(-1));
    assert_eq!(1, zigzag_varint_len(63));
    assert_eq!(2, zigzag_varint_len(64));

    let mut stats = FieldStats::new(0);
    for value in [10_000, 10_001, 9_999] {
        stats.observe(value);
    }

    assert!(stats.previous_delta_zigzag_varint_bytes < 3 * 8);
    assert!(stats.delta2_zigzag_varint_bytes <= stats.previous_delta_zigzag_varint_bytes + 1);
    assert!(stats.absolute_zigzag_varint_bytes > stats.previous_delta_zigzag_varint_bytes);
}
