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
        assert!(json["canonical_hash_mode"].as_str().is_some());
        assert!(!json["guard_mode"].as_str().unwrap().is_empty());
        assert!(json["working_tree_dirty"].as_bool().is_some());
        assert!(json["records_per_sec"].as_f64().unwrap() > 0.0);
        assert!(json["input_mb_per_sec"].as_f64().unwrap() > 0.0);
        assert!(json["mb_per_sec"].as_f64().unwrap() > 0.0);
        assert!(json["result_path"].is_null());
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
            assert_eq!(true, json["compiled_plan_used"]);
            assert!(json["conversion_plan_hash"].as_u64().unwrap() > 0);
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

#[test]
fn aura0_to_aura1_cursor_decode_path_declines_unsupported_fixture() {
    let (aura0, _) = partitioned_sparse_fixture_profiles();
    let cursor = records::try_compile_i64_file_profiled(
        &aura0,
        Profile::Aura1,
        records::OutputGuardMode::OldPostOutputGuard,
        records::TranscodePath::Direct,
        records::Aura0EncoderPath::Materialized,
        records::Aura0DecodePath::Cursor,
    )
    .unwrap();

    assert!(
        cursor.is_none(),
        "cursor path must decline unsupported fixture instead of silently falling back"
    );
}

#[test]
fn aura0_to_aura1_profiled_generic_writer_matches_reference_rows() {
    let (aura0, aura1) = fixture_profiles();
    let profiled = records::try_compile_i64_file_profiled(
        &aura0,
        Profile::Aura1,
        records::OutputGuardMode::OldPostOutputGuard,
        records::TranscodePath::Auto,
        records::Aura0EncoderPath::Materialized,
        records::Aura0DecodePath::Materialized,
    )
    .unwrap()
    .expect("generic profiled aura0 to aura1 path");

    assert!(profiled.conversion_plan_hash.is_some());
    let timings = profiled
        .timings
        .aura0_to_aura1
        .expect("aura0 to aura1 timings");
    assert!(timings.total_ns > 0);
    assert!(timings.decode_input_streams.total_ns > 0);
    assert!(timings.partitioned_sparse_writer.total_ns > 0);
    assert!(timings.partitioned_sparse_writer.output_byte_stores_ns > 0);
    assert_eq!(
        records::decode_i64_file(&aura1).unwrap().rows,
        records::decode_i64_file(&profiled.bytes).unwrap().rows
    );
}

#[test]
fn aura_bench_reports_decode_path_field() {
    let Some(bin) = option_env!("CARGO_BIN_EXE_aura-bench") else {
        panic!("missing aura-bench binary");
    };

    let dir = std::env::temp_dir().join(format!(
        "aura-bench-decode-path-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let (aura0, _) = fixture_profiles();
    let aura0_path = dir.join("fixture.aura0");
    fs::write(&aura0_path, aura0).unwrap();

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
        .arg("--decode-path")
        .arg("materialized")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!("materialized", json["decode_path_requested"]);
    assert_eq!("materialized", json["decode_path"]);

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn aura_bench_reports_aura1_to_aura0_direct_transcode_path() {
    let Some(bin) = option_env!("CARGO_BIN_EXE_aura-bench") else {
        panic!("missing aura-bench binary");
    };

    let dir = std::env::temp_dir().join(format!(
        "aura-bench-direct-aura0-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let (_, aura1) = partitioned_sparse_fixture_profiles();
    let expected_record_count = records::decode_i64_file(&aura1).unwrap().rows.len() as u64;
    let aura1_path = dir.join("fixture.aura1");
    fs::write(&aura1_path, aura1).unwrap();

    let output = Command::new(bin)
        .arg("--operation")
        .arg("transcode-aura1-to-aura0")
        .arg("--dataset")
        .arg("unit-fixture")
        .arg("--input")
        .arg(&aura1_path)
        .arg("--iterations")
        .arg("1")
        .arg("--format")
        .arg("json")
        .arg("--transcode-path")
        .arg("direct")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!("direct", json["transcode_path_requested"]);
    assert_eq!("direct", json["transcode_path"]);
    assert_eq!("transcode-aura1-to-aura0", json["operation"]);
    assert_eq!(expected_record_count, json["record_count"]);
    assert_eq!(true, json["compiled_plan_used"]);
    assert!(json["conversion_plan_hash"].as_u64().unwrap() > 0);

    let timings = &json["stage_timings_ns"]["aura1_to_aura0"];
    assert!(timings["total"].as_u64().unwrap() > 0);
    assert!(timings["fixed_row_scan"].as_u64().unwrap() > 0);
    assert!(timings["compression_encoding"].as_u64().unwrap() > 0);
    let child_sum = [
        "metadata",
        "fixed_row_scan",
        "stats_frequency_collection",
        "direct_stream_construction",
        "field_extraction",
        "dictionary_state_update",
        "delta_stream_construction",
        "compression_encoding",
        "writer_finalization",
        "canonical_hash",
        "post_output_guard",
    ]
    .into_iter()
    .map(|field| timings[field].as_u64().unwrap())
    .sum::<u64>();
    assert!(child_sum <= timings["total"].as_u64().unwrap());

    let stats = &json["aura1_to_aura0_stats"];
    assert_eq!(
        expected_record_count,
        stats["rows_scanned"].as_u64().unwrap()
    );
    assert_eq!(0, stats["row_allocations"].as_u64().unwrap());
    assert!(stats["stream_vector_allocations"].as_u64().unwrap() > 0);
    assert!(stats["bytes_read"].as_u64().unwrap() > 0);
    assert!(stats["bytes_written"].as_u64().unwrap() > 0);

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn aura1_to_aura0_direct_path_matches_materialized_output() {
    let (_, aura1) = partitioned_sparse_fixture_profiles();
    let materialized = records::compile_i64_file(&aura1, Profile::Aura0).unwrap();
    let direct = records::try_compile_i64_file_profiled(
        &aura1,
        Profile::Aura0,
        records::OutputGuardMode::OldPostOutputGuard,
        records::TranscodePath::Direct,
        records::Aura0EncoderPath::Materialized,
        records::Aura0DecodePath::Materialized,
    )
    .unwrap()
    .expect("direct aura1 to aura0 path");

    assert_eq!(records::TranscodePath::Direct, direct.transcode_path);
    assert!(direct.output_byte_guard.is_some());
    assert_eq!(materialized, direct.bytes);

    let materialized_rows = records::decode_i64_file(&materialized).unwrap().rows;
    let direct_rows = records::decode_i64_file(&direct.bytes).unwrap().rows;
    assert_eq!(materialized_rows, direct_rows);

    let stats = direct.stats.aura1_to_aura0.expect("direct stats");
    assert_eq!(materialized_rows.len(), stats.rows_scanned);
    assert_eq!(0, stats.row_allocations);
    assert!(stats.stream_vector_allocations > 0);
}

#[test]
fn aura1_to_aura0_direct_stream_encoder_matches_current_direct_path() {
    let (_, aura1) = partitioned_sparse_fixture_profiles();
    let materialized = records::compile_i64_file(&aura1, Profile::Aura0).unwrap();
    let direct_column = records::try_compile_i64_file_profiled(
        &aura1,
        Profile::Aura0,
        records::OutputGuardMode::OldPostOutputGuard,
        records::TranscodePath::Direct,
        records::Aura0EncoderPath::Materialized,
        records::Aura0DecodePath::Materialized,
    )
    .unwrap()
    .expect("direct column aura1 to aura0 path");
    let direct_streams = records::try_compile_i64_file_profiled(
        &aura1,
        Profile::Aura0,
        records::OutputGuardMode::OldPostOutputGuard,
        records::TranscodePath::Direct,
        records::Aura0EncoderPath::DirectStreams,
        records::Aura0DecodePath::Materialized,
    )
    .unwrap()
    .expect("direct stream aura1 to aura0 path");

    assert_eq!(materialized, direct_column.bytes);
    assert_eq!(direct_column.bytes, direct_streams.bytes);
    assert_eq!(
        direct_column.output_byte_guard,
        direct_streams.output_byte_guard
    );

    let reference_rows = records::decode_i64_file(&materialized).unwrap().rows;
    let direct_stream_rows = records::decode_i64_file(&direct_streams.bytes)
        .unwrap()
        .rows;
    assert_eq!(reference_rows, direct_stream_rows);

    let stats = direct_streams
        .stats
        .aura1_to_aura0
        .expect("direct stream stats");
    assert!(stats.direct_streams_enabled);
    assert_eq!(records::Aura0EncoderPath::DirectStreams, stats.encoder_path);
    assert_eq!(reference_rows.len(), stats.rows_scanned);
    assert_eq!(0, stats.row_allocations);
    assert_eq!(0, stats.stream_vector_allocations);
    assert!(stats.direct_stream_count > 0);
    assert!(stats.direct_stream_value_count > 0);
}

#[test]
fn aura_bench_reports_aura1_to_aura0_direct_stream_encoder_path() {
    let Some(bin) = option_env!("CARGO_BIN_EXE_aura-bench") else {
        panic!("missing aura-bench binary");
    };

    let dir = std::env::temp_dir().join(format!(
        "aura-bench-direct-streams-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let (_, aura1) = partitioned_sparse_fixture_profiles();
    let expected_record_count = records::decode_i64_file(&aura1).unwrap().rows.len() as u64;
    let aura1_path = dir.join("fixture.aura1");
    fs::write(&aura1_path, aura1).unwrap();

    let output = Command::new(bin)
        .arg("--operation")
        .arg("transcode-aura1-to-aura0")
        .arg("--dataset")
        .arg("unit-fixture")
        .arg("--input")
        .arg(&aura1_path)
        .arg("--iterations")
        .arg("1")
        .arg("--format")
        .arg("json")
        .arg("--transcode-path")
        .arg("direct")
        .arg("--encoder-path")
        .arg("direct-streams")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!("direct", json["transcode_path"]);
    assert_eq!("direct-streams", json["encoder_path_requested"]);
    assert_eq!("direct-streams", json["encoder_path"]);
    assert_eq!(true, json["direct_streams_enabled"]);
    assert_eq!(expected_record_count, json["record_count"]);

    let stats = &json["aura1_to_aura0_stats"];
    assert_eq!(
        expected_record_count,
        stats["rows_scanned"].as_u64().unwrap()
    );
    assert_eq!(0, stats["row_allocations"].as_u64().unwrap());
    assert_eq!(0, stats["stream_vector_allocations"].as_u64().unwrap());
    assert!(stats["direct_stream_count"].as_u64().unwrap() > 0);
    assert!(stats["direct_stream_value_count"].as_u64().unwrap() > 0);
    assert!(stats["bytes_written"].as_u64().unwrap() > 0);

    let timings = &json["stage_timings_ns"]["aura1_to_aura0"];
    assert!(timings["direct_stream_construction"].as_u64().unwrap() > 0);

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn aura_bench_rejects_column_free_encoder_with_current_api_blocker() {
    let Some(bin) = option_env!("CARGO_BIN_EXE_aura-bench") else {
        panic!("missing aura-bench binary");
    };

    let dir = std::env::temp_dir().join(format!(
        "aura-bench-column-free-reject-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let (_, aura1) = partitioned_sparse_fixture_profiles();
    let aura1_path = dir.join("fixture.aura1");
    fs::write(&aura1_path, aura1).unwrap();

    let output = Command::new(bin)
        .arg("--operation")
        .arg("transcode-aura1-to-aura0")
        .arg("--dataset")
        .arg("unit-fixture")
        .arg("--input")
        .arg(&aura1_path)
        .arg("--iterations")
        .arg("1")
        .arg("--format")
        .arg("json")
        .arg("--transcode-path")
        .arg("direct")
        .arg("--encoder-path")
        .arg("column-free")
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "column-free must fail explicitly until encoder API no longer requires columns"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("column-free encoder rejected")
            && stderr.contains("requires Aura1 column buffers"),
        "stderr:\n{stderr}"
    );

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn aura_bench_preserves_and_verifies_transcode_output() {
    let Some(bin) = option_env!("CARGO_BIN_EXE_aura-bench") else {
        panic!("missing aura-bench binary");
    };

    let dir = std::env::temp_dir().join(format!(
        "aura-bench-preserve-output-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let (_, aura1) = partitioned_sparse_fixture_profiles();
    let source_rows = records::decode_i64_file(&aura1).unwrap().rows;
    let aura1_path = dir.join("fixture.aura1");
    let preserved_path = dir.join("preserved.aura0");
    fs::write(&aura1_path, aura1).unwrap();

    let output = Command::new(bin)
        .arg("--operation")
        .arg("transcode-aura1-to-aura0")
        .arg("--dataset")
        .arg("unit-fixture")
        .arg("--input")
        .arg(&aura1_path)
        .arg("--iterations")
        .arg("1")
        .arg("--format")
        .arg("json")
        .arg("--transcode-path")
        .arg("direct")
        .arg("--encoder-path")
        .arg("direct-streams")
        .arg("--guard-mode")
        .arg("old_post_output_guard")
        .arg("--preserve-output")
        .arg(&preserved_path)
        .arg("--verify-output-decodes")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(true, json["output_preserved"]);
    assert_eq!(preserved_path.display().to_string(), json["output_path"]);
    assert_eq!(true, json["decoded_row_equality"]);
    assert_eq!(true, json["record_count_equality"]);
    assert_eq!(true, json["schema_footer_validation"]);
    assert_eq!(true, json["output_byte_guard_equality"]);
    assert!(json["conversion_plan_hash"].as_u64().unwrap() > 0);
    assert!(json["output_verification_runtime_ns"].as_u64().unwrap() > 0);

    let preserved = fs::read(&preserved_path).unwrap();
    let preserved_rows = records::decode_i64_file(&preserved).unwrap().rows;
    assert_eq!(source_rows, preserved_rows);

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn aura_bench_reports_transcode_canonical_hash_verify_mode() {
    let Some(bin) = option_env!("CARGO_BIN_EXE_aura-bench") else {
        panic!("missing aura-bench binary");
    };

    let dir = std::env::temp_dir().join(format!(
        "aura-bench-canonical-hash-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let (_, aura1) = partitioned_sparse_fixture_profiles();
    let aura1_path = dir.join("fixture.aura1");
    let preserved_path = dir.join("canonical.aura0");
    fs::write(&aura1_path, aura1).unwrap();

    let output = Command::new(bin)
        .arg("--operation")
        .arg("transcode-aura1-to-aura0")
        .arg("--dataset")
        .arg("unit-fixture")
        .arg("--input")
        .arg(&aura1_path)
        .arg("--iterations")
        .arg("1")
        .arg("--format")
        .arg("json")
        .arg("--transcode-path")
        .arg("direct")
        .arg("--encoder-path")
        .arg("direct-streams")
        .arg("--canonical-hash-mode")
        .arg("verify")
        .arg("--preserve-output")
        .arg(&preserved_path)
        .arg("--verify-output-decodes")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!("verify", json["canonical_hash_mode"]);
    assert!(json["canonical_hash"].as_u64().unwrap() > 0);
    assert!(json["canonical_hash_time_ms"].as_f64().unwrap() >= 0.0);
    assert_eq!(true, json["canonical_hash_equality"]);

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn aura_bench_reports_aura1_replay_operations() {
    let Some(bin) = option_env!("CARGO_BIN_EXE_aura-bench") else {
        panic!("missing aura-bench binary");
    };

    let dir = std::env::temp_dir().join(format!(
        "aura-bench-aura1-replay-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let (_, aura1) = partitioned_sparse_fixture_profiles();
    let aura1_path = dir.join("fixture.aura1");
    fs::write(&aura1_path, aura1).unwrap();

    for operation in [
        "aura1-scan-fixed",
        "aura1-replay-callback",
        "aura1-parse-to-rows",
    ] {
        let output = Command::new(bin)
            .arg("--operation")
            .arg(operation)
            .arg("--dataset")
            .arg("unit-fixture")
            .arg("--input")
            .arg(&aura1_path)
            .arg("--iterations")
            .arg("1")
            .arg("--format")
            .arg("json")
            .arg("--canonical-hash-mode")
            .arg("verify")
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "operation: {operation}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(operation, json["operation"]);
        assert_eq!("aura1", json["source_format"]);
        assert_eq!("none", json["target_format"]);
        assert_eq!(true, json["compiled_plan_used"]);
        assert!(json["conversion_plan_hash"].as_u64().unwrap() > 0);
        assert!(json["record_count"].as_u64().unwrap() > 0);
        assert!(json["records_per_sec"].as_f64().unwrap() > 0.0);
        assert!(json["replay_stats"]["record_width"].as_u64().unwrap() > 0);
        assert!(json["replay_stats"]["bytes_scanned"].as_u64().unwrap() > 0);
        assert!(json["canonical_hash"].as_u64().unwrap() > 0);
        if operation == "aura1-parse-to-rows" {
            assert!(
                json["replay_stats"]["materialized_row_count"]
                    .as_u64()
                    .unwrap()
                    > 0
            );
        } else {
            assert_eq!(
                0,
                json["replay_stats"]["materialized_row_count"]
                    .as_u64()
                    .unwrap()
            );
        }
    }

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn aura_bench_reports_compiled_plan_for_aura1_canonical_scan() {
    let Some(bin) = option_env!("CARGO_BIN_EXE_aura-bench") else {
        panic!("missing aura-bench binary");
    };

    let dir = std::env::temp_dir().join(format!(
        "aura-bench-plan-canonical-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let (_, aura1) = partitioned_sparse_fixture_profiles();
    let aura1_path = dir.join("fixture.aura1");
    fs::write(&aura1_path, aura1).unwrap();

    let output = Command::new(bin)
        .arg("--operation")
        .arg("parse-aura1")
        .arg("--dataset")
        .arg("unit-fixture")
        .arg("--input")
        .arg(&aura1_path)
        .arg("--iterations")
        .arg("1")
        .arg("--format")
        .arg("json")
        .arg("--canonical-hash-mode")
        .arg("verify")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(true, json["compiled_plan_used"]);
    assert_eq!("compiled", json["plan_mode"]);
    assert!(json["conversion_plan_hash"].as_u64().unwrap() > 0);
    assert!(json["plan_setup_time_ms"].as_f64().unwrap() >= 0.0);
    assert!(json["canonical_hash"].as_u64().unwrap() > 0);

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn aura_bench_reports_zstd_baselines() {
    let Some(bin) = option_env!("CARGO_BIN_EXE_aura-bench") else {
        panic!("missing aura-bench binary");
    };

    let dir = std::env::temp_dir().join(format!(
        "aura-bench-zstd-baseline-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let (_, aura1) = partitioned_sparse_fixture_profiles();
    let aura1_path = dir.join("fixture.aura1");
    fs::write(&aura1_path, aura1).unwrap();

    for operation in [
        "zstd-decompress-only",
        "zstd-decompress-plus-parse",
        "zstd-decompress-plus-replay",
    ] {
        let output = Command::new(bin)
            .arg("--operation")
            .arg(operation)
            .arg("--dataset")
            .arg("unit-fixture")
            .arg("--input")
            .arg(&aura1_path)
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
        assert_eq!(operation, json["operation"]);
        assert_eq!("zstd", json["baseline_kind"]);
        assert!(json["compressed_input_bytes"].as_u64().unwrap() > 0);
        assert!(json["decompressed_output_bytes"].as_u64().unwrap() > 0);
        assert!(json["work_included"].as_str().unwrap().contains("zstd"));
        assert!(json["records_per_sec"].as_f64().unwrap() >= 0.0);
    }

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn aura_bench_reports_fair_aura0_vs_zstd_bytes_benchmarks() {
    let Some(bin) = option_env!("CARGO_BIN_EXE_aura-bench") else {
        panic!("missing aura-bench binary");
    };

    let dir =
        std::env::temp_dir().join(format!("aura-bench-fair-zstd-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let (aura0, aura1) = partitioned_sparse_fixture_profiles();
    let aura0_path = dir.join("fixture.aura0");
    let aura1_path = dir.join("fixture.aura1");
    fs::write(&aura0_path, aura0).unwrap();
    fs::write(&aura1_path, aura1).unwrap();

    for (operation, input) in [
        ("aura0-to-aura1-bytes", &aura0_path),
        ("aura0-to-aura1-bytes-verify", &aura0_path),
        ("zstd-aura1-to-aura1-bytes", &aura1_path),
        ("zstd-aura1-to-aura1-bytes-verify", &aura1_path),
    ] {
        let output = Command::new(bin)
            .arg("--operation")
            .arg(operation)
            .arg("--dataset")
            .arg("unit-fixture")
            .arg("--input")
            .arg(input)
            .arg("--reference-aura0")
            .arg(&aura0_path)
            .arg("--reference-aura1")
            .arg(&aura1_path)
            .arg("--iterations")
            .arg("1")
            .arg("--format")
            .arg("json")
            .arg("--zstd-level")
            .arg("1")
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "operation: {operation}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(operation, json["operation"]);
        assert_eq!("memory_vec", json["output_sink"]);
        assert_eq!("no_guard", json["guard_mode"]);
        assert_eq!("none", json["canonical_hash_mode"]);
        assert_eq!(1, json["zstd_level"]);
        assert!(json["dataset_sha256_aura0"].as_str().unwrap().len() == 64);
        assert!(json["dataset_sha256_aura1"].as_str().unwrap().len() == 64);
        assert!(json["dataset_sha256_aura1_zst"].as_str().unwrap().len() == 64);
        assert!(json["aura0_compressed_bytes"].as_u64().unwrap() > 0);
        assert!(json["aura1_zstd_compressed_bytes"].as_u64().unwrap() > 0);
        assert!(json["aura1_uncompressed_bytes"].as_u64().unwrap() > 0);
        assert!(json["compressed_input_mb_sec"].as_f64().unwrap() > 0.0);
        assert!(json["uncompressed_output_mb_sec"].as_f64().unwrap() > 0.0);

        if operation.ends_with("-verify") {
            assert_eq!(true, json["output_bytes_equal"]);
            assert!(json["output_byte_hash"].as_u64().unwrap() > 0);
        } else {
            assert!(json["output_bytes_equal"].is_null());
            assert!(json["output_byte_hash"].is_null());
        }
    }

    fs::remove_dir_all(&dir).unwrap();
}
