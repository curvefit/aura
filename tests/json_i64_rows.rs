use std::fs;
use std::process::Command;

use aura_codec::{
    records, DerivedExpression, DerivedExpressionOp, DerivedOp, GenericGroupInstruction, Profile,
};

const SCHEMA_HEADER: &str = "100,0,2,2,2,0,1,0,0,6,8";
const SCHEMA_BYTES: &[u8] = &[100, 0, 2, 2, 2, 0, 1, 0, 0, 6, 8];
const DERIVED_SCHEMA_HEADER: &str = "100,0,102,103,2,0,1,0,0,6,8";
const DERIVED_SCHEMA_BYTES: &[u8] = &[100, 0, 102, 103, 2, 0, 1, 0, 0, 6, 8];

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
        .arg(DERIVED_SCHEMA_HEADER)
        .arg("--derive")
        .arg("2:max_plus_residual:2:1,4")
        .arg("--derive")
        .arg("3:min_minus_residual:3:1,4")
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
    assert!(stdout.contains("derived_expressions=2"));

    let expected_derives = vec![
        DerivedExpression::new(2, 2, DerivedExpressionOp::MaxPlusResidual, vec![1, 4]).unwrap(),
        DerivedExpression::new(3, 3, DerivedExpressionOp::MinMinusResidual, vec![1, 4]).unwrap(),
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

    fs::remove_dir_all(&dir).unwrap();
}
