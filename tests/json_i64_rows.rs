use std::fs;
use std::process::Command;

use aura_codec::{
    records, AuraI64EventReader, DerivedExpression, DerivedExpressionOp, DerivedOp,
    GenericGroupInstruction, Profile,
};

const SCHEMA_HEADER: &str = "100,0,2,2,2,0,1,0,0,6,8";
const SCHEMA_BYTES: &[u8] = &[100, 0, 2, 2, 2, 0, 1, 0, 0, 6, 8];
const DERIVED_SCHEMA_HEADER: &str = "100,101,102,103,2,0,1,107,0,6,110";
const DERIVED_SCHEMA_BYTES: &[u8] = &[100, 101, 102, 103, 2, 0, 1, 107, 0, 6, 110];

#[test]
fn json_explicit_events_retain_independent_order_count_slot() {
    let Some(bin) = option_env!("CARGO_BIN_EXE_aura-json-i64") else {
        panic!("missing aura-json-i64 binary");
    };
    let dir = std::env::temp_dir().join(format!(
        "aura-json-i64-explicit-qty2-count-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let input = dir.join("events.json");
    let output = dir.join("count-related.aura");
    fs::write(
        &input,
        r#"[
            {"event":[1700000000000,42],"children":[
                [0,"100.01","8.5","8.0",3],
                [1,"100.02","7.0","7.0",9]
            ]},
            {"event":[1700000000100,43],"children":[
                [0,"100.00","9.25","8.75",11]
            ]}
        ]"#,
    )
    .unwrap();

    let result = Command::new(bin)
        .arg("--events")
        .arg("--schema")
        .arg("100,0,200,205,0,0,5,0")
        .arg("--decimal-scale")
        .arg("100")
        .arg("--timestamp-multiplier")
        .arg("1")
        .arg("--out")
        .arg(&output)
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );

    let bytes = fs::read(output.with_extension("aura0")).unwrap();
    let reader = AuraI64EventReader::open(&bytes).unwrap();
    assert_eq!(
        reader.header().schema_mapping,
        [100, 0, 200, 205, 0, 0, 5, 0]
    );
    assert_eq!(reader.events()[0].children[0], [0, 10_001, 850, 800, 3]);
    assert_eq!(reader.events()[0].children[1][4], 9);
    assert_eq!(reader.events()[1].children[0][4], 11);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn json_explicit_events_use_compact_operation200_parent_residual_schema() {
    let Some(bin) = option_env!("CARGO_BIN_EXE_aura-json-i64") else {
        panic!("missing aura-json-i64 binary");
    };
    let dir = std::env::temp_dir().join(format!(
        "aura-json-i64-explicit-qty2-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let input = dir.join("events.json");
    let related_output = dir.join("related.aura");
    let direct_output = dir.join("direct.aura");
    let mut document = String::from("[");
    for event in 0..256i64 {
        if event != 0 {
            document.push(',');
        }
        document.push_str(&format!(
            "{{\"event\":[{},{}],\"children\":[",
            1_000_000 + event * 100,
            event
        ));
        for child in 0..64i64 {
            if child != 0 {
                document.push(',');
            }
            let side = child & 1;
            let price = 50_000_000 + child * 10 + (event * 17).rem_euclid(31);
            let mixed = i128::from(event * 64 + child) * 6_364_136_223_846_793_005i128;
            let total =
                1_000_000_000_000 + i64::try_from(mixed.rem_euclid(8_000_000_000_000i128)).unwrap();
            let qty2 = total - (event + child).rem_euclid(5);
            document.push_str(&format!("[{side},{price},{total},{qty2}]"));
        }
        document.push_str("]}");
    }
    document.push(']');
    fs::write(&input, document).unwrap();

    let run = |schema: &str, output: &std::path::Path| {
        Command::new(bin)
            .arg("--events")
            .arg("--schema")
            .arg(schema)
            .arg("--timestamp-multiplier")
            .arg("1")
            .arg("--out")
            .arg(output)
            .arg(&input)
            .output()
            .unwrap()
    };
    let related = run("100,0,200,204,0,0,5", &related_output);
    assert!(
        related.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&related.stdout),
        String::from_utf8_lossy(&related.stderr)
    );
    let direct = run("100,0,200,204,0,0,0", &direct_output);
    assert!(direct.status.success());

    let related_bytes = fs::read(related_output.with_extension("aura0")).unwrap();
    let direct_bytes = fs::read(direct_output.with_extension("aura0")).unwrap();
    assert!(related_bytes.len() * 100 < direct_bytes.len() * 75);
    let decoded = records::decode_i64_events_file(&related_bytes).unwrap();
    assert_eq!(decoded.events.len(), 256);
    assert_eq!(
        AuraI64EventReader::open(&related_bytes)
            .unwrap()
            .events()
            .iter()
            .map(|event| event.children.len())
            .sum::<usize>(),
        16_384
    );
    let plan = decoded.compiled_footer.unwrap().generic_aura0_plan.unwrap();
    assert!(plan.groups.iter().any(|group| matches!(
        group,
        GenericGroupInstruction::DerivedStream {
            output_slot: 5,
            input_slots,
            ..
        } if input_slots == &[4]
    )));
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn json_positional_rows_support_structural_dual_domain_control() {
    let Some(bin) = option_env!("CARGO_BIN_EXE_aura-json-i64") else {
        panic!("missing aura-json-i64 binary");
    };

    let dir = std::env::temp_dir().join(format!("aura-json-i64-dual-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let input = dir.join("book.json");
    let output = dir.join("book.aura");
    fs::write(&input, r#"[[1000,0,"100.0","2.0"],[1000,1,"100.1","3.0"]]"#).unwrap();

    let result = Command::new(bin)
        .arg("--schema")
        .arg("100,200,203,2,0")
        .arg("--timestamp-multiplier")
        .arg("1")
        .arg("--out")
        .arg(&output)
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("slots=4"));

    for path in [
        output.clone(),
        output.with_extension("aura0"),
        output.with_extension("aura1"),
    ] {
        let decoded = records::decode_i64_file(&fs::read(path).unwrap()).unwrap();
        assert_eq!(
            &[100, 200, 203, 2, 0],
            decoded.header.schema_mapping.as_slice()
        );
        assert_eq!(
            vec![vec![1000, 0, 1000, 2], vec![1000, 1, 1001, 3]],
            decoded.rows
        );
    }

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn json_positional_rows_encode_compile_and_decode_from_schema_header() {
    let Some(bin) = option_env!("CARGO_BIN_EXE_aura-json-i64") else {
        panic!("missing aura-json-i64 binary");
    };

    let dir = std::env::temp_dir().join(format!("aura-json-i64-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let input = dir.join("rows.json");
    let output = dir.join("rows.aura");
    fs::write(
        &input,
        r#"[
            [1000, "10.12000000", "10.25000000", "9.75000000", "10.10000000", "1.50001000", 1999, "15.12345670", 3, "1.00001000", "10.12345670", "0"],
            [2000, "10.10000000", "10.50000000", "10.00000000", "10.25000000", "2.00000000", 2999, "20.50000000", 4, "0.50000000", "5.12500000", "0"]
        ]"#,
    )
    .unwrap();

    let output_result = Command::new(bin)
        .arg("--schema")
        .arg(SCHEMA_HEADER)
        .arg("--out")
        .arg(&output)
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        output_result.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output_result.stdout),
        String::from_utf8_lossy(&output_result.stderr)
    );
    let stdout = String::from_utf8_lossy(&output_result.stdout);
    assert!(stdout.contains("timestamp_multiplier=1000000"));
    assert!(stdout.contains(
        "decimal_scales=[1, 100, 100, 100, 100, 100000, 1, 10000000, 1, 100000, 10000000]"
    ));

    let expected = vec![
        vec![
            1_000_000_000,
            1012,
            1025,
            975,
            1010,
            150_001,
            1_999_000_000,
            151_234_567,
            3,
            100_001,
            101_234_567,
        ],
        vec![
            2_000_000_000,
            1010,
            1050,
            1000,
            1025,
            200_000,
            2_999_000_000,
            205_000_000,
            4,
            50_000,
            51_250_000,
        ],
    ];

    let aura = records::decode_i64_file(&fs::read(&output).unwrap()).unwrap();
    let aura0 =
        records::decode_i64_file(&fs::read(output.with_extension("aura0")).unwrap()).unwrap();
    let aura1 =
        records::decode_i64_file(&fs::read(output.with_extension("aura1")).unwrap()).unwrap();

    assert_eq!(Profile::Ingest, aura.header.profile);
    assert_eq!(Profile::Aura0, aura0.header.profile);
    assert_eq!(Profile::Aura1, aura1.header.profile);
    assert_eq!(SCHEMA_BYTES, aura.header.schema_mapping.as_slice());
    assert_eq!(SCHEMA_BYTES, aura0.header.schema_mapping.as_slice());
    assert_eq!(SCHEMA_BYTES, aura1.header.schema_mapping.as_slice());
    assert_eq!(expected, aura.rows);
    assert_eq!(expected, aura0.rows);
    assert_eq!(expected, aura1.rows);

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn json_positional_rows_accept_derived_expression_schema_header() {
    let Some(bin) = option_env!("CARGO_BIN_EXE_aura-json-i64") else {
        panic!("missing aura-json-i64 binary");
    };

    let dir =
        std::env::temp_dir().join(format!("aura-json-i64-derived-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let input = dir.join("binance-klines.json");
    let output = dir.join("binance-derived.aura");
    let mut rows_json = String::from("[\n");
    let mut previous_close = 10_000i64;
    for index in 0..128 {
        let open_time = 1_704_067_200_000i64 + i64::from(index) * 60_000;
        let open = previous_close;
        let close = open + (i64::from(index % 7) - 3) * 10;
        let high = open.max(close) + i64::from(index % 2);
        let low = open.min(close) - i64::from(index % 3);
        let volume = 1_000 + i64::from((index * 13) % 200);
        let close_time = open_time + 59_999;
        let quote_volume = volume * close + i64::from(index % 5);
        let trade_count = 100 + i64::from(index % 17);
        let taker_buy_base = volume / 3;
        let taker_buy_quote = close * taker_buy_base + i64::from((index * 3) % 7);
        previous_close = close;
        rows_json.push_str(&format!(
            "  [{open_time}, \"{open}.00\", \"{high}.00\", \"{low}.00\", \"{close}.00\", \"{volume}.00000\", {close_time}, \"{quote_volume}.0000000\", {trade_count}, \"{taker_buy_base}.00000\", \"{taker_buy_quote}.0000000\", \"0\"]"
        ));
        if index + 1 != 128 {
            rows_json.push_str(",\n");
        }
    }
    rows_json.push_str("\n]");
    fs::write(&input, rows_json).unwrap();

    let output_result = Command::new(bin)
        .arg("--schema")
        .arg(DERIVED_SCHEMA_HEADER)
        .arg("--derive")
        .arg("1:first_offset_then_delta:1:4")
        .arg("--derive")
        .arg("2:max_plus_residual:2:1,4")
        .arg("--derive")
        .arg("3:min_minus_residual:3:1,4")
        .arg("--derive")
        .arg("7:mul:7:4,5")
        .arg("--derive")
        .arg("10:mul:10:4,9")
        .arg("--out")
        .arg(&output)
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        output_result.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output_result.stdout),
        String::from_utf8_lossy(&output_result.stderr)
    );
    let stdout = String::from_utf8_lossy(&output_result.stdout);
    assert!(stdout.contains("derived_expressions=5"));

    let expected_derives = vec![
        DerivedExpression::new(1, 1, DerivedExpressionOp::FirstOffsetThenDelta, vec![4]).unwrap(),
        DerivedExpression::new(2, 2, DerivedExpressionOp::MaxPlusResidual, vec![1, 4]).unwrap(),
        DerivedExpression::new(3, 3, DerivedExpressionOp::MinMinusResidual, vec![1, 4]).unwrap(),
        DerivedExpression::new(7, 7, DerivedExpressionOp::Mul, vec![4, 5]).unwrap(),
        DerivedExpression::new(10, 10, DerivedExpressionOp::Mul, vec![4, 9]).unwrap(),
    ];

    let aura = records::decode_i64_file(&fs::read(&output).unwrap()).unwrap();
    let aura0 =
        records::decode_i64_file(&fs::read(output.with_extension("aura0")).unwrap()).unwrap();
    let aura1 =
        records::decode_i64_file(&fs::read(output.with_extension("aura1")).unwrap()).unwrap();

    for decoded in [&aura, &aura0, &aura1] {
        assert_eq!(
            DERIVED_SCHEMA_BYTES,
            decoded.header.schema_mapping.as_slice()
        );
        assert_eq!(expected_derives, decoded.header.derived_expressions);
    }

    let plan = aura
        .ingest_footer
        .as_ref()
        .unwrap()
        .generic_aura0_plan
        .as_ref()
        .unwrap();
    assert!(plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::DerivedStream {
                output_slot: 1,
                op: DerivedOp::FirstOffsetThenDelta,
                input_slots,
                ..
            } if input_slots.as_slice() == [4]
        )
    }));
    assert!(plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::DerivedStream {
                output_slot: 2,
                op: DerivedOp::MaxPlusResidual,
                input_slots,
                ..
            } if input_slots.as_slice() == [1, 4]
        )
    }));
    assert!(plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::DerivedStream {
                output_slot: 3,
                op: DerivedOp::MinMinusResidual,
                input_slots,
                ..
            } if input_slots.as_slice() == [1, 4]
        )
    }));
    assert!(plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::ExpressionStream {
                output_slot: 7,
                op: DerivedExpressionOp::Mul,
                input_slots,
                ..
            } if input_slots.as_slice() == [4, 5]
        )
    }));
    assert!(plan.groups.iter().any(|group| {
        matches!(
            group,
            GenericGroupInstruction::ExpressionStream {
                output_slot: 10,
                op: DerivedExpressionOp::Mul,
                input_slots,
                ..
            } if input_slots.as_slice() == [4, 9]
        )
    }));

    fs::remove_dir_all(&dir).unwrap();
}
