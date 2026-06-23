use std::fs;
use std::process::Command;

use aura_codec::records;
use serde_json::Value;

#[test]
fn aura_fixture_gen_writes_compatible_fixture_matrix() {
    let Some(bin) = option_env!("CARGO_BIN_EXE_aura-fixture-gen") else {
        panic!("missing aura-fixture-gen binary");
    };
    let dir = std::env::temp_dir().join(format!("aura-fixture-gen-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let output = Command::new(bin)
        .arg("--output-dir")
        .arg(&dir)
        .arg("--zstd-level")
        .arg("3")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "fixture generator failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let metadata_path = dir.join("fixtures.json");
    let metadata: Value = serde_json::from_slice(&fs::read(&metadata_path).unwrap()).unwrap();
    let fixtures = metadata.as_array().expect("fixture metadata array");
    let names = fixtures
        .iter()
        .map(|fixture| fixture["dataset_name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "tiny",
            "dense-few-symbol",
            "sparse-many-symbol",
            "huff",
            "sdk-tiny",
            "sdk-narrow",
            "sdk-wide",
            "sdk-reordered",
            "sdk-dense",
            "sdk-sparse",
            "sdk-edge-case",
            "sdk-larger",
            "nohuff",
            "larger"
        ]
    );

    for fixture in fixtures {
        if fixture["coverage_status"] == "blocked_by_specific_format_issue" {
            assert_eq!("huff", fixture["dataset_name"]);
            assert!(fixture["blocker"]
                .as_str()
                .unwrap()
                .contains("HuffmanDictionary"));
            continue;
        }

        let aura0_path = fixture["paths"]["aura0"].as_str().unwrap();
        let aura1_path = fixture["paths"]["aura1"].as_str().unwrap();
        let aura1_zst_path = fixture["paths"]["aura1_zst"].as_str().unwrap();
        assert_eq!(true, fixture["row_equality_verified"]);
        assert!(fixture["record_count"].as_u64().unwrap() > 0);
        assert!(fixture["schema_hash"].as_u64().unwrap() > 0);
        assert!(fixture["schema_name"].as_str().unwrap().len() > 0);
        assert_eq!(
            fixture["field_count"].as_u64().unwrap() as usize,
            fixture["field_names"].as_array().unwrap().len()
        );
        assert!(fixture["record_width"].as_u64().unwrap() > 0);
        assert_eq!(64, fixture["aura0_sha256"].as_str().unwrap().len());
        assert_eq!(64, fixture["aura1_sha256"].as_str().unwrap().len());
        assert_eq!(64, fixture["aura1_zst_sha256"].as_str().unwrap().len());
        assert!(fixture["aura0_bytes"].as_u64().unwrap() > 0);
        assert!(fixture["aura1_bytes"].as_u64().unwrap() > 0);
        assert!(fixture["aura1_zst_bytes"].as_u64().unwrap() > 0);
        let aura0 = fs::read(aura0_path).unwrap();
        let aura1 = fs::read(aura1_path).unwrap();
        assert!(fs::metadata(aura1_zst_path).unwrap().len() > 0);
        let decoded_aura0 = records::decode_i64_file(&aura0).unwrap();
        let decoded_aura1 = records::decode_i64_file(&aura1).unwrap();
        assert_eq!(decoded_aura0.rows, decoded_aura1.rows);
    }

    let huff = fixtures
        .iter()
        .find(|fixture| fixture["dataset_name"] == "huff")
        .unwrap();
    assert_eq!("blocked_by_specific_format_issue", huff["coverage_status"]);
    let nohuff = fixtures
        .iter()
        .find(|fixture| fixture["dataset_name"] == "nohuff")
        .unwrap();
    assert_eq!(0, nohuff["huffman_stream_count"].as_u64().unwrap());

    let smoke_path = dir.join("sdk_bench_smoke.json");
    let smoke: Value = serde_json::from_slice(&fs::read(&smoke_path).unwrap()).unwrap();
    assert_eq!("sdk-generic-smoke", smoke["matrix_kind"]);
    let entries = smoke["entries"].as_array().unwrap();
    assert!(entries.len() >= 21);
    assert!(entries
        .iter()
        .any(|entry| entry["dataset_kind"] == "sdk-reordered"
            && entry["operation"] == "aura0-to-aura1-bytes"));
    for entry in entries {
        assert!(entry["dataset_kind"].as_str().unwrap().starts_with("sdk-"));
        assert!(entry["schema_hash"].as_u64().unwrap() > 0);
        assert!(entry["field_count"].as_u64().unwrap() > 0);
        assert!(entry["record_width"].as_u64().unwrap() > 0);
        assert!(entry["command"].as_array().unwrap().len() > 8);
    }
}
