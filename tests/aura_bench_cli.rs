use std::fs;
use std::process::Command;

use aura_codec::schema::{generic_i64_parent_schema, ohlcv_schema};
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

fn partitioned_sparse_fixture_profiles() -> (Vec<u8>, Vec<u8>) {
    let schema =
        generic_i64_parent_schema("parented_repeated", &[100, 0, 0, 205, 4, 5, 5, 5]).unwrap();
    let rows = (0..96)
        .flat_map(|event| {
            let run_sizes = [
                2 + usize::from(event % 3 == 0),
                3 + usize::from(event % 4 == 0),
            ];
            run_sizes
                .into_iter()
                .enumerate()
                .flat_map(move |(partition, run_len)| {
                    (0..run_len).map(move |level| {
                        let base_price = 2_000_000 + i64::from(event) * 10;
                        let first_price = if partition == 0 {
                            base_price - 100
                        } else {
                            base_price + 8_000_000 + 100
                        };
                        let price = first_price + i64::try_from(level).unwrap() * 5;
                        let has_qty = (event + level as u16 + partition as u16).is_multiple_of(11);
                        vec![
                            1_000_000 + i64::from(event / 2),
                            10_000 + i64::from(event),
                            20_000 + i64::from(event / 3),
                            partition as i64,
                            price,
                            if has_qty { 1_000 + i64::from(event) } else { 0 },
                            if has_qty {
                                2_000 + i64::from(level as u16)
                            } else {
                                0
                            },
                            i64::from(has_qty),
                        ]
                    })
                })
        })
        .collect::<Vec<_>>();

    let ingest = records::encode_ingest_i64_file(records::I64FileInput {
        schema,
        rows,
        stream_id: 17,
        dictionary_id: 29,
        header_comment: Some("aura bench partitioned sparse fixture".to_owned()),
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

    for (operation, input, source, target, writes_output, verifies_rows) in [
        ("parse-aura1", &aura1_path, "aura1", "none", false, true),
        ("decode-aura0", &aura0_path, "aura0", "none", false, true),
        (
            "transcode-aura1-to-aura0",
            &aura1_path,
            "aura1",
            "aura0",
            true,
            false,
        ),
        (
            "transcode-aura0-to-aura1",
            &aura0_path,
            "aura0",
            "aura1",
            true,
            false,
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
        assert!(json["total_runtime_ns"].as_u64().unwrap() >= json["runtime_ns"].as_u64().unwrap());
        assert!(
            json["median_total_runtime_ns"].as_u64().unwrap()
                >= json["median_runtime_ns"].as_u64().unwrap()
        );
        assert!(
            json["p95_total_runtime_ns"].as_u64().unwrap()
                >= json["p95_runtime_ns"].as_u64().unwrap()
        );
        assert!(json["post_process_runtime_ns"].as_u64().is_some());
        assert!(json["median_post_process_runtime_ns"].as_u64().is_some());
        assert!(json["p95_post_process_runtime_ns"].as_u64().is_some());
        assert!(json["post_process_note"]
            .as_str()
            .unwrap()
            .contains("runtime_ns excludes it"));
        if verifies_rows {
            assert!(json["canonical_hash"].as_u64().is_some());
        } else {
            assert!(json["canonical_hash"].is_null());
        }
        assert!(!json["guard_mode"].as_str().unwrap().is_empty());
        assert!(json["records_per_sec"].as_f64().unwrap() > 0.0);
        assert!(json["mb_per_sec"].as_f64().unwrap() > 0.0);
        assert!(json["command_used"].as_str().unwrap().contains(operation));
        assert!(json["git_commit"].as_str().unwrap().len() >= 7);
        assert!(json["machine_info"]["os"].as_str().unwrap().len() > 0);
    }

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn aura_bench_reports_aura0_to_aura1_guard_modes_and_stage_tree() {
    let Some(bin) = option_env!("CARGO_BIN_EXE_aura-bench") else {
        panic!("missing aura-bench binary");
    };

    let dir = std::env::temp_dir().join(format!(
        "aura-bench-guard-modes-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let (aura0, _) = partitioned_sparse_fixture_profiles();
    let aura0_path = dir.join("fixture.aura0");
    fs::write(&aura0_path, aura0).unwrap();

    for (mode, expects_output_guard) in [
        ("no_guard", false),
        ("fused_output_guard", true),
        ("old_post_output_guard", true),
        ("block_batched_output_guard", true),
    ] {
        let output = Command::new(bin)
            .arg("--operation")
            .arg("transcode-aura0-to-aura1")
            .arg("--dataset")
            .arg("unit-fixture")
            .arg("--input")
            .arg(&aura0_path)
            .arg("--iterations")
            .arg("1")
            .arg("--format")
            .arg("json")
            .arg("--guard-mode")
            .arg(mode)
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "mode: {mode}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(mode, json["guard_mode_requested"]);
        let actual_mode = json["guard_mode"].as_str().unwrap();
        assert!(
            actual_mode == mode || actual_mode == "post_process_output",
            "requested mode {mode}, actual mode {actual_mode}"
        );
        if expects_output_guard {
            assert!(json["output_byte_guard"].as_u64().is_some());
        } else {
            assert!(json["output_byte_guard"].is_null());
        }
        let decode_stats = &json["decode_stats"];
        let stream_count = decode_stats["stream_count"].as_u64().unwrap();
        let stream_value_count = decode_stats["stream_value_count"].as_u64().unwrap();
        let materialized_stream_count = decode_stats["materialized_stream_count"].as_u64().unwrap();
        let materialized_value_count = decode_stats["materialized_value_count"].as_u64().unwrap();
        let profiled_path = json["stage_timings_ns"]["total"].as_u64().unwrap_or(0) > 0;
        if profiled_path {
            assert!(stream_count > 0);
            assert!(stream_value_count > 0);
            assert!(materialized_stream_count > 0);
            assert!(materialized_value_count > 0);
        }
        assert_eq!(
            0,
            decode_stats["direct_cursor_stream_count"].as_u64().unwrap()
        );
        assert_eq!(
            0,
            decode_stats["direct_cursor_value_count"].as_u64().unwrap()
        );

        let stages = &json["stage_timings_ns"];
        let decode = &stages["decode_input_streams"];
        let decode_total = decode["total"].as_u64().unwrap();
        let decode_children = [
            "compressed_input_read",
            "huffman_entropy_decode",
            "delta_reconstruction",
            "dictionary_symbol_reconstruction",
            "validity_presence_bitmap_decode",
            "record_type_branching",
            "integer_scaling_sign_extension",
            "temporary_buffer_writes",
            "checksum_hash_work",
            "bounds_validation",
            "allocation_reuse",
            "unclassified",
        ]
        .into_iter()
        .map(|field| decode[field].as_u64().unwrap())
        .sum::<u64>();
        assert_eq!(decode_total, decode_children);

        let writer = &stages["partitioned_sparse_writer"];
        let writer_total = writer["total"].as_u64().unwrap();
        let writer_children = [
            "sparse_partition_traversal",
            "output_offset_calculation",
            "field_reconstruction_packing",
            "output_byte_stores",
            "guard_hash_update",
            "bounds_checks",
            "branch_record_type_handling",
            "copy_cost",
            "buffer_slicing_view_creation",
            "partition_finalization",
            "allocation_reuse",
            "unclassified",
        ]
        .into_iter()
        .map(|field| writer[field].as_u64().unwrap())
        .sum::<u64>();
        assert_eq!(writer_total, writer_children);
    }

    fs::remove_dir_all(&dir).unwrap();
}
