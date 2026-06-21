use std::fs;
use std::process::Command;

use aura_codec::schema::ohlcv_schema;
use aura_codec::{records, Profile};

fn fixture_rows() -> Vec<Vec<i64>> {
    (0..16)
        .map(|index| {
            let ts = 1_700_000_000_000_000_000 + i64::from(index) * 60_000_000_000;
            let open = 10_000 + i64::from(index % 5);
            let high = open + 25;
            let low = open - 20;
            let close = open + i64::from(index % 3) - 1;
            let volume = 1_000 + i64::from(index * 7);
            vec![ts, open, high, low, close, volume]
        })
        .collect()
}

fn fixture_profiles() -> (Vec<u8>, Vec<u8>) {
    let rows = fixture_rows();
    let ingest = records::encode_ingest_i64_file(records::I64FileInput {
        schema: ohlcv_schema().unwrap(),
        rows,
        stream_id: 17,
        dictionary_id: 29,
        header_comment: Some("aura bench fixture".to_owned()),
    })
    .unwrap();

    let aura0 = records::compile_i64_file(&ingest, Profile::Aura0).unwrap();
    let aura1 = records::compile_i64_file(&ingest, Profile::Aura1).unwrap();
    (aura0, aura1)
}

#[test]
fn aura_bench_reports_required_json_fields_for_core_operations() {
    let Some(bin) = option_env!("CARGO_BIN_EXE_aura-bench") else {
        panic!("missing aura-bench binary");
    };

    let dir = std::env::temp_dir().join(format!("aura-bench-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let (aura0, aura1) = fixture_profiles();
    let aura0_path = dir.join("fixture.aura0");
    let aura1_path = dir.join("fixture.aura1");
    fs::write(&aura0_path, aura0).unwrap();
    fs::write(&aura1_path, aura1).unwrap();

    for (operation, input, source, target, writes_output) in [
        ("parse-aura1", &aura1_path, "aura1", "none", false),
        ("decode-aura0", &aura0_path, "aura0", "none", false),
        (
            "transcode-aura1-to-aura0",
            &aura1_path,
            "aura1",
            "aura0",
            true,
        ),
        (
            "transcode-aura0-to-aura1",
            &aura0_path,
            "aura0",
            "aura1",
            true,
        ),
    ] {
        let output = Command::new(bin)
            .arg("--operation")
            .arg(operation)
            .arg("--dataset")
            .arg("unit-fixture")
            .arg("--input")
            .arg(input)
            .arg("--iterations")
            .arg("1")
            .arg("--format")
            .arg("json")
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "operation: {operation}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!("unit-fixture", json["dataset_name"]);
        assert_eq!(operation, json["operation"]);
        assert_eq!(source, json["source_format"]);
        assert_eq!(target, json["target_format"]);
        assert_eq!(16, json["record_count"]);
        assert!(json["dataset_sha256"].as_str().unwrap().len() == 64);
        assert!(json["input_bytes"].as_u64().unwrap() > 0);
        if writes_output {
            assert!(json["output_bytes"].as_u64().unwrap() > 0);
            assert!(json["compression_ratio"].as_f64().unwrap() > 0.0);
        } else {
            assert_eq!(0, json["output_bytes"].as_u64().unwrap());
            assert!(json["compression_ratio"].is_null());
        }
        assert!(json["runtime_ns"].as_u64().unwrap() > 0);
        assert!(json["median_runtime_ns"].as_u64().unwrap() > 0);
        assert!(json["p95_runtime_ns"].as_u64().unwrap() > 0);
        assert!(json["records_per_sec"].as_f64().unwrap() > 0.0);
        assert!(json["mb_per_sec"].as_f64().unwrap() > 0.0);
        assert!(json["command_used"].as_str().unwrap().contains(operation));
        assert!(json["git_commit"].as_str().unwrap().len() >= 7);
        assert!(json["machine_info"]["os"].as_str().unwrap().len() > 0);
    }

    fs::remove_dir_all(&dir).unwrap();
}
