use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use aura_codec::records::{self, I64FileInput, OutputGuardMode, TranscodePath};
use aura_codec::schema::generic_i64_parent_schema;
use aura_codec::writer;
use aura_codec::{
    Aura0ColumnPath, Aura1BodyPath, Aura1ExecutionOptions, AuraI64EventWriter, DerivedExpression,
    DerivedExpressionOp, FieldRole, FieldType, I64Event, Profile, SchemaBuilder,
    UnsupportedPathBehavior,
};

fn input(name: &str, rows: Vec<Vec<i64>>) -> I64FileInput {
    I64FileInput {
        schema: generic_i64_parent_schema(name, &[100, 0, 0]).unwrap(),
        rows,
        stream_id: 7,
        dictionary_id: 9,
        header_comment: Some("execution-strategy-test".to_owned()),
    }
}

fn bytes_guard(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325u64, |acc, byte| {
        acc.wrapping_mul(0x100000001b3)
            .wrapping_add(u64::from(*byte))
    })
}

fn compile_with(
    aura0: &[u8],
    body_path: Aura1BodyPath,
    column_path: Aura0ColumnPath,
    unsupported_path: UnsupportedPathBehavior,
) -> aura_codec::Result<Option<records::ProfiledCompileWithOptionsOutput>> {
    compile_with_guard(
        aura0,
        body_path,
        column_path,
        unsupported_path,
        OutputGuardMode::NoGuard,
    )
}

fn compile_with_guard(
    aura0: &[u8],
    body_path: Aura1BodyPath,
    column_path: Aura0ColumnPath,
    unsupported_path: UnsupportedPathBehavior,
    guard_mode: OutputGuardMode,
) -> aura_codec::Result<Option<records::ProfiledCompileWithOptionsOutput>> {
    records::try_compile_i64_file_profiled_with_options(
        aura0,
        Profile::Aura1,
        guard_mode,
        TranscodePath::Auto,
        Aura1ExecutionOptions {
            body_path,
            column_path,
            unsupported_path,
        },
    )
}

fn compile_with_lane_use(
    aura0: &[u8],
    body_path: Aura1BodyPath,
    unsupported_path: UnsupportedPathBehavior,
    lane_use: records::Aura0ByteLaneUse,
) -> aura_codec::Result<Option<records::ProfiledCompileWithOptionsOutput>> {
    records::try_compile_i64_file_profiled_with_options_and_lane_use(
        aura0,
        Profile::Aura1,
        OutputGuardMode::NoGuard,
        TranscodePath::Auto,
        Aura1ExecutionOptions {
            body_path,
            column_path: Aura0ColumnPath::Materialized,
            unsupported_path,
        },
        lane_use,
    )
}

#[test]
fn supported_explicit_body_paths_are_byte_identical_to_stable_auto() {
    let rows = vec![
        vec![i64::MIN, 0, i64::MAX],
        vec![-1, 1, 0],
        vec![0, -1, 1],
        vec![i64::MAX, i64::MIN, -1],
    ];
    let ingest = writer::encode_i64(input("execution_extremes_v1", rows.clone())).unwrap();
    let aura0 = writer::compile_i64(&ingest, Profile::Aura0).unwrap();
    let stable = writer::compile_i64(&aura0, Profile::Aura1).unwrap();

    let mut supported = 0usize;
    for path in [
        Aura1BodyPath::DirectFile,
        Aura1BodyPath::StreamingCursor,
        Aura1BodyPath::DirectStreams,
        Aura1BodyPath::Columns,
    ] {
        let output = match compile_with(
            &aura0,
            path,
            Aura0ColumnPath::Materialized,
            UnsupportedPathBehavior::Error,
        ) {
            Ok(Some(output)) => {
                supported += 1;
                output
            }
            Err(aura_codec::AuraError::InvalidValue("requested Aura1 body path unsupported")) => {
                compile_with(
                    &aura0,
                    path,
                    Aura0ColumnPath::Materialized,
                    UnsupportedPathBehavior::FallbackToStable,
                )
                .unwrap()
                .unwrap_or_else(|| panic!("{path:?} stable fallback declined"))
            }
            Ok(None) => panic!("{path:?} returned an untyped unsupported result"),
            Err(error) => panic!("{path:?} failed unexpectedly: {error}"),
        };
        assert_eq!(path, output.execution.requested_body_path);
        if output.execution.effective_body_path.as_str() == path.as_str() {
            assert_eq!(None, output.execution.fallback_reason);
        } else {
            assert!(output.execution.fallback_reason.is_some());
        }
        assert_eq!(
            stable, output.profiled.bytes,
            "{path:?} changed Aura1 bytes"
        );
        assert_eq!(
            rows,
            records::decode_i64_file(&output.profiled.bytes)
                .unwrap()
                .rows
        );
    }
    assert!(supported >= 2, "too few explicit paths exercised directly");
}

#[test]
fn unsupported_exact_path_errors_or_records_stable_fallback() {
    let expressions =
        vec![
            DerivedExpression::new(1, 1, DerivedExpressionOp::FirstOffsetThenDelta, vec![2])
                .unwrap(),
        ];
    let schema = generic_i64_parent_schema("execution_derived_v1", &[100, 101, 2])
        .unwrap()
        .with_derived_expressions(expressions)
        .unwrap();
    let rows = (0..64)
        .map(|index| vec![index * 1_000, 10_000 + index, 9_999 + index])
        .collect::<Vec<_>>();
    let ingest = writer::encode_i64(I64FileInput {
        schema,
        rows,
        stream_id: 1,
        dictionary_id: 2,
        header_comment: None,
    })
    .unwrap();
    let aura0 = writer::compile_i64(&ingest, Profile::Aura0).unwrap();
    let stable = writer::compile_i64(&aura0, Profile::Aura1).unwrap();

    let error = compile_with(
        &aura0,
        Aura1BodyPath::StreamingCursor,
        Aura0ColumnPath::Materialized,
        UnsupportedPathBehavior::Error,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        aura_codec::AuraError::InvalidValue("requested Aura1 body path unsupported")
    ));

    let fallback = compile_with(
        &aura0,
        Aura1BodyPath::StreamingCursor,
        Aura0ColumnPath::Materialized,
        UnsupportedPathBehavior::FallbackToStable,
    )
    .unwrap()
    .expect("stable fallback");
    assert_eq!(
        Aura1BodyPath::StreamingCursor,
        fallback.execution.requested_body_path
    );
    assert_ne!(
        fallback.execution.requested_body_path.as_str(),
        fallback.execution.effective_body_path.as_str()
    );
    assert_eq!(
        Some("requested Aura1 body path unsupported"),
        fallback.execution.fallback_reason
    );
    assert_eq!(stable, fallback.profiled.bytes);
}

#[test]
fn typed_execution_options_are_safe_to_use_concurrently() {
    let ingest = writer::encode_i64(input(
        "execution_concurrent_v1",
        (0..512i64)
            .map(|index| vec![index * 10, index.rem_euclid(2), -index])
            .collect(),
    ))
    .unwrap();
    let aura0 = writer::compile_i64(&ingest, Profile::Aura0).unwrap();
    let stable = writer::compile_i64(&aura0, Profile::Aura1).unwrap();

    let handles = [
        Aura1BodyPath::DirectFile,
        Aura1BodyPath::StreamingCursor,
        Aura1BodyPath::DirectStreams,
        Aura1BodyPath::Columns,
    ]
    .into_iter()
    .map(|path| {
        let aura0 = aura0.clone();
        thread::spawn(move || {
            compile_with(
                &aura0,
                path,
                Aura0ColumnPath::Materialized,
                UnsupportedPathBehavior::FallbackToStable,
            )
            .unwrap()
            .unwrap()
            .profiled
            .bytes
        })
    })
    .collect::<Vec<_>>();

    for handle in handles {
        assert_eq!(stable, handle.join().unwrap());
    }
}

#[test]
fn real_materialized_compiler_decodes_rows_for_compact_hybrid_and_fast() {
    let rows = (0..256i64)
        .map(|index| vec![index * 1_000, index.rem_euclid(11), -index])
        .collect::<Vec<_>>();
    let ingest = writer::encode_i64(input("real_materialized_v1", rows.clone())).unwrap();
    let aura1 = writer::compile_i64(&ingest, Profile::Aura1).unwrap();
    let compact = writer::compile_i64(&ingest, Profile::Aura0).unwrap();
    let hybrid = records::compile_i64_file_with_aura0_profile(
        &aura1,
        records::Aura0FileProfile::Hybrid,
        records::Aura0ByteLaneCodec::Lz4,
    )
    .unwrap();
    let fast = records::compile_i64_file_with_aura0_profile(
        &aura1,
        records::Aura0FileProfile::Fast,
        records::Aura0ByteLaneCodec::Lz4,
    )
    .unwrap();

    for source in [&compact, &hybrid, &fast] {
        let materialized = records::compile_aura0_to_aura1_materialized(source).unwrap();
        assert_eq!(rows.len(), materialized.rows_materialized);
        assert_eq!(3, materialized.fields_materialized);
        assert_eq!(rows.len() * 3, materialized.values_materialized);
        assert_eq!(aura1, materialized.bytes);
        let decoded = records::decode_i64_file(&materialized.bytes).unwrap();
        assert_eq!(rows, decoded.rows);
        assert!(decoded
            .compiled_footer
            .expect("compiled footer")
            .aura1_byte_lanes
            .is_empty());
    }
}

#[test]
fn exact_guard_modes_are_never_misreported() {
    assert_eq!(
        UnsupportedPathBehavior::Error,
        Aura1ExecutionOptions::default().unsupported_path,
        "omitting fallback policy must fail closed"
    );
    let ingest = writer::encode_i64(input(
        "execution_guard_v1",
        vec![vec![1, 2, 3], vec![2, 3, 4], vec![3, 4, 5]],
    ))
    .unwrap();
    let aura0 = writer::compile_i64(&ingest, Profile::Aura0).unwrap();
    let stable = writer::compile_i64(&aura0, Profile::Aura1).unwrap();

    for guard_mode in [
        OutputGuardMode::NoGuard,
        OutputGuardMode::OldPostOutputGuard,
    ] {
        let output = compile_with_guard(
            &aura0,
            Aura1BodyPath::DirectFile,
            Aura0ColumnPath::Materialized,
            UnsupportedPathBehavior::Error,
            guard_mode,
        )
        .unwrap()
        .expect("exact guard-supported path");
        assert_eq!(guard_mode, output.profiled.guard_mode);
        assert_eq!("direct-file", output.execution.effective_body_path.as_str());
        assert_eq!(stable, output.profiled.bytes);
        assert_eq!(
            guard_mode == OutputGuardMode::OldPostOutputGuard,
            output.profiled.output_byte_guard.is_some()
        );
    }

    for guard_mode in [
        OutputGuardMode::FusedOutputGuard,
        OutputGuardMode::BlockBatchedOutputGuard,
    ] {
        let error = compile_with_guard(
            &aura0,
            Aura1BodyPath::DirectFile,
            Aura0ColumnPath::Materialized,
            UnsupportedPathBehavior::Error,
            guard_mode,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            aura_codec::AuraError::InvalidValue(
                "requested Aura1 body path does not support guard mode"
            )
        ));

        let fallback = compile_with_guard(
            &aura0,
            Aura1BodyPath::DirectFile,
            Aura0ColumnPath::Materialized,
            UnsupportedPathBehavior::FallbackToStable,
            guard_mode,
        )
        .unwrap()
        .expect("stable profiled guard fallback");
        assert_eq!(guard_mode, fallback.profiled.guard_mode);
        assert!(fallback.profiled.output_byte_guard.is_some());
        assert_ne!(
            "direct-file",
            fallback.execution.effective_body_path.as_str()
        );
        assert_eq!(
            Some("requested Aura1 body path does not support guard mode"),
            fallback.execution.fallback_reason
        );
        assert_eq!(stable, fallback.profiled.bytes);
    }

    let hybrid = records::compile_i64_file_with_aura0_profile(
        &stable,
        records::Aura0FileProfile::Hybrid,
        records::Aura0ByteLaneCodec::Lz4,
    )
    .unwrap();
    for guard_mode in [
        OutputGuardMode::FusedOutputGuard,
        OutputGuardMode::BlockBatchedOutputGuard,
    ] {
        let fallback = compile_with_guard(
            &hybrid,
            Aura1BodyPath::DirectFile,
            Aura0ColumnPath::Materialized,
            UnsupportedPathBehavior::FallbackToStable,
            guard_mode,
        )
        .unwrap()
        .expect("lane-backed stable guard fallback");
        assert_eq!(guard_mode, fallback.profiled.guard_mode);
        assert_ne!(
            aura_codec::Aura1EffectiveBodyPath::EmbeddedByteLane,
            fallback.execution.effective_body_path
        );
        assert_eq!(stable, fallback.profiled.bytes);
        let timings = fallback
            .profiled
            .timings
            .aura0_to_aura1
            .expect("Aura0 to Aura1 timings");
        assert_eq!(0, timings.post_output_guard_ns);
        assert!(fallback.profiled.output_byte_guard.is_some());
    }
    let legacy_guarded =
        records::try_compile_i64_file_with_fused_output_guard(&hybrid, Profile::Aura1)
            .unwrap()
            .expect("hybrid semantic fused guard");
    assert_eq!(stable, legacy_guarded.bytes);
    assert_eq!(bytes_guard(&stable), legacy_guarded.guard);

    let fast = records::compile_i64_file_with_aura0_profile(
        &stable,
        records::Aura0FileProfile::Fast,
        records::Aura0ByteLaneCodec::Lz4,
    )
    .unwrap();
    let error = compile_with_guard(
        &fast,
        Aura1BodyPath::DirectFile,
        Aura0ColumnPath::Materialized,
        UnsupportedPathBehavior::FallbackToStable,
        OutputGuardMode::FusedOutputGuard,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        aura_codec::AuraError::InvalidValue("stable Aura1 fallback unsupported")
    ));
    assert!(
        records::try_compile_i64_file_with_fused_output_guard(&fast, Profile::Aura1)
            .unwrap()
            .is_none(),
        "pure byte-lane output cannot truthfully claim a fused semantic guard"
    );

    for guard_mode in [
        OutputGuardMode::FusedOutputGuard,
        OutputGuardMode::BlockBatchedOutputGuard,
    ] {
        for semantic_input in [&aura0, &hybrid] {
            let strict = compile_with_guard(
                semantic_input,
                Aura1BodyPath::StableAuto,
                Aura0ColumnPath::Materialized,
                UnsupportedPathBehavior::Error,
                guard_mode,
            )
            .unwrap()
            .expect("stable semantic guard");
            assert_eq!(guard_mode, strict.profiled.guard_mode);
            assert_ne!(
                aura_codec::Aura1EffectiveBodyPath::EmbeddedByteLane,
                strict.execution.effective_body_path
            );
            assert_eq!(stable, strict.profiled.bytes);
        }

        let strict_error = compile_with_guard(
            &fast,
            Aura1BodyPath::StableAuto,
            Aura0ColumnPath::Materialized,
            UnsupportedPathBehavior::Error,
            guard_mode,
        )
        .unwrap_err();
        assert!(matches!(
            strict_error,
            aura_codec::AuraError::InvalidValue("requested stable Aura1 guard/path unsupported")
        ));

        let honest_fallback = compile_with_guard(
            &fast,
            Aura1BodyPath::StableAuto,
            Aura0ColumnPath::Materialized,
            UnsupportedPathBehavior::FallbackToStable,
            guard_mode,
        )
        .unwrap()
        .expect("old-post lane fallback");
        assert_eq!(
            OutputGuardMode::OldPostOutputGuard,
            honest_fallback.profiled.guard_mode
        );
        assert_eq!(
            aura_codec::Aura1EffectiveBodyPath::EmbeddedByteLane,
            honest_fallback.execution.effective_body_path
        );
        assert_eq!(
            Some("requested guard mode unsupported; used old post-output guard"),
            honest_fallback.execution.fallback_reason
        );
        assert!(
            honest_fallback
                .profiled
                .timings
                .aura0_to_aura1
                .expect("Aura0 timings")
                .post_output_guard_ns
                > 0
        );
        assert_eq!(stable, honest_fallback.profiled.bytes);

        let never_fallback = records::try_compile_i64_file_profiled_with_options_and_lane_use(
            &fast,
            Profile::Aura1,
            guard_mode,
            TranscodePath::Auto,
            Aura1ExecutionOptions {
                body_path: Aura1BodyPath::StableAuto,
                column_path: Aura0ColumnPath::Materialized,
                unsupported_path: UnsupportedPathBehavior::FallbackToStable,
            },
            records::Aura0ByteLaneUse::Never,
        )
        .unwrap_err();
        assert!(matches!(
            never_fallback,
            aura_codec::AuraError::InvalidValue("stable Aura1 fallback unsupported")
        ));
    }
}

#[test]
fn byte_lane_zero_row_and_grouped_inputs_keep_stable_bytes() {
    let zero_ingest = writer::encode_i64(input("execution_zero_v1", Vec::new())).unwrap();
    let zero_aura0 = writer::compile_i64(&zero_ingest, Profile::Aura0).unwrap();
    let zero_stable = writer::compile_i64(&zero_aura0, Profile::Aura1).unwrap();
    for path in [
        Aura1BodyPath::DirectFile,
        Aura1BodyPath::StreamingCursor,
        Aura1BodyPath::DirectStreams,
        Aura1BodyPath::Columns,
    ] {
        let output = compile_with(
            &zero_aura0,
            path,
            Aura0ColumnPath::Materialized,
            UnsupportedPathBehavior::FallbackToStable,
        )
        .unwrap()
        .expect("zero-row conversion");
        assert_eq!(zero_stable, output.profiled.bytes);
    }

    let grouped_schema =
        generic_i64_parent_schema("execution_grouped_sparse_v1", &[100, 0, 0, 205, 4, 5, 5, 5])
            .unwrap();
    let grouped_rows = vec![
        vec![1_000, 10, 20, 0, 100_000, 5, 0, 0],
        vec![1_000, 10, 20, 0, 100_010, 0, 7, 1],
        vec![1_000, 10, 20, 1, 100_020, 9, 0, 0],
        vec![2_000, 11, 21, 0, 100_030, 0, 8, 1],
        vec![2_000, 11, 21, 1, 100_040, 11, 0, 0],
    ];
    let grouped_ingest = writer::encode_i64(I64FileInput {
        schema: grouped_schema,
        rows: grouped_rows.clone(),
        stream_id: 3,
        dictionary_id: 4,
        header_comment: None,
    })
    .unwrap();
    let grouped_aura0 = writer::compile_i64(&grouped_ingest, Profile::Aura0).unwrap();
    let grouped_stable = writer::compile_i64(&grouped_aura0, Profile::Aura1).unwrap();
    for column_path in [
        Aura0ColumnPath::Materialized,
        Aura0ColumnPath::PartitionedSparseCursor,
    ] {
        let output = compile_with(
            &grouped_aura0,
            Aura1BodyPath::Columns,
            column_path,
            UnsupportedPathBehavior::FallbackToStable,
        )
        .unwrap()
        .expect("grouped stable fallback");
        assert_eq!(grouped_stable, output.profiled.bytes);
        assert_eq!(
            grouped_rows,
            records::decode_i64_file(&output.profiled.bytes)
                .unwrap()
                .rows
        );
    }

    let fast = records::compile_i64_file_with_aura0_profile(
        &grouped_stable,
        records::Aura0FileProfile::Fast,
        records::Aura0ByteLaneCodec::Lz4,
    )
    .unwrap();
    let lane = compile_with(
        &fast,
        Aura1BodyPath::StableAuto,
        Aura0ColumnPath::Materialized,
        UnsupportedPathBehavior::Error,
    )
    .unwrap()
    .expect("stable embedded byte lane");
    assert_eq!(
        aura_codec::Aura1EffectiveBodyPath::EmbeddedByteLane,
        lane.execution.effective_body_path
    );
    assert_eq!(grouped_stable, lane.profiled.bytes);

    for path in [
        Aura1BodyPath::DirectFile,
        Aura1BodyPath::StreamingCursor,
        Aura1BodyPath::DirectStreams,
        Aura1BodyPath::Columns,
    ] {
        let error = compile_with(
            &fast,
            path,
            Aura0ColumnPath::PartitionedSparseCursor,
            UnsupportedPathBehavior::Error,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            aura_codec::AuraError::InvalidValue("requested Aura1 body path unsupported")
        ));
    }

    let hybrid = records::compile_i64_file_with_aura0_profile(
        &grouped_stable,
        records::Aura0FileProfile::Hybrid,
        records::Aura0ByteLaneCodec::Lz4,
    )
    .unwrap();
    let never = compile_with_lane_use(
        &hybrid,
        Aura1BodyPath::StableAuto,
        UnsupportedPathBehavior::Error,
        records::Aura0ByteLaneUse::Never,
    )
    .unwrap()
    .expect("semantic stable path");
    assert_ne!(
        aura_codec::Aura1EffectiveBodyPath::EmbeddedByteLane,
        never.execution.effective_body_path
    );
    assert_eq!(grouped_stable, never.profiled.bytes);
    let always = compile_with_lane_use(
        &hybrid,
        Aura1BodyPath::StableAuto,
        UnsupportedPathBehavior::Error,
        records::Aura0ByteLaneUse::Always,
    )
    .unwrap()
    .expect("required embedded lane");
    assert_eq!(
        aura_codec::Aura1EffectiveBodyPath::EmbeddedByteLane,
        always.execution.effective_body_path
    );
    let always_exact = compile_with_lane_use(
        &hybrid,
        Aura1BodyPath::DirectStreams,
        UnsupportedPathBehavior::Error,
        records::Aura0ByteLaneUse::Always,
    )
    .unwrap_err();
    assert!(matches!(
        always_exact,
        aura_codec::AuraError::InvalidValue(
            "embedded byte lane conflicts with requested semantic path"
        )
    ));
    let mut semantic_dispatches = 0usize;
    for path in [
        Aura1BodyPath::DirectFile,
        Aura1BodyPath::StreamingCursor,
        Aura1BodyPath::DirectStreams,
        Aura1BodyPath::Columns,
    ] {
        match compile_with(
            &hybrid,
            path,
            Aura0ColumnPath::Materialized,
            UnsupportedPathBehavior::Error,
        ) {
            Ok(Some(output)) => {
                semantic_dispatches += 1;
                assert_eq!(path.as_str(), output.execution.effective_body_path.as_str());
                assert_ne!(
                    aura_codec::Aura1EffectiveBodyPath::EmbeddedByteLane,
                    output.execution.effective_body_path
                );
                assert_eq!(grouped_stable, output.profiled.bytes);
            }
            Err(aura_codec::AuraError::InvalidValue("requested Aura1 body path unsupported")) => {}
            Ok(None) => panic!("{path:?} returned an untyped unsupported result"),
            Err(error) => panic!("{path:?} failed unexpectedly: {error}"),
        }
    }
    assert!(semantic_dispatches >= 1);
}

#[test]
fn production_library_sources_do_not_reference_removed_ambient_switches() {
    let source_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let removed = [
        ["AURA", "STREAM", "AURA1"].join("_"),
        ["AURA", "FORCE", "COLUMNS", "AURA1"].join("_"),
        ["AURA", "DIRECT", "AURA1"].join("_"),
        ["AURA", "CURSOR", "COLUMNS"].join("_"),
        ["AURA", "PROFILE", "FAST"].join("_"),
    ];
    let mut pending = vec![source_root];
    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let source = fs::read_to_string(&path).unwrap();
                for name in &removed {
                    assert!(!source.contains(name), "{} contains {name}", path.display());
                }
            }
        }
    }
}

#[test]
fn removed_environment_switch_values_do_not_change_production_output() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "aura-execution-env-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir_all(&root).unwrap();
    let ingest = writer::encode_i64(input(
        "execution_subprocess_v1",
        vec![vec![1, 2, 3], vec![4, 5, 6], vec![7, 8, 9]],
    ))
    .unwrap();
    let aura0 = writer::compile_i64(&ingest, Profile::Aura0).unwrap();
    let input_path = root.join("input.aura0");
    fs::write(&input_path, aura0).unwrap();
    let names = [
        ["AURA", "STREAM", "AURA1"].join("_"),
        ["AURA", "FORCE", "COLUMNS", "AURA1"].join("_"),
        ["AURA", "DIRECT", "AURA1"].join("_"),
        ["AURA", "CURSOR", "COLUMNS"].join("_"),
        ["AURA", "PROFILE", "FAST"].join("_"),
    ];
    let variants = [None, Some(""), Some("0"), Some("1"), Some("λ-Aura")];
    let mut expected = None;
    for (index, value) in variants.into_iter().enumerate() {
        let output_path = root.join(format!("output-{index}.aura1"));
        let mut command = Command::new(env!("CARGO_BIN_EXE_aura-bench"));
        command
            .args([
                "--operation",
                "transcode-aura0-to-aura1",
                "--dataset",
                "environment-invariance",
                "--input",
            ])
            .arg(&input_path)
            .args(["--iterations", "1", "--warmups", "0", "--preserve-output"])
            .arg(&output_path);
        for name in &names {
            command.env_remove(name);
            if let Some(value) = value {
                command.env(name, value);
            }
        }
        let result = command.output().unwrap();
        assert!(
            result.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(result.stderr.is_empty(), "library emitted diagnostics");
        let bytes = fs::read(output_path).unwrap();
        if let Some(expected) = &expected {
            assert_eq!(
                expected, &bytes,
                "environment variant {value:?} changed bytes"
            );
        } else {
            expected = Some(bytes);
        }
    }
}

#[test]
fn benchmark_json_reports_requested_and_effective_typed_paths() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "aura-execution-json-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir_all(&root).unwrap();
    let ingest = writer::encode_i64(input(
        "execution_json_v1",
        vec![vec![1, 2, 3], vec![2, 3, 4]],
    ))
    .unwrap();
    let input_path = root.join("input.aura0");
    fs::write(
        &input_path,
        writer::compile_i64(&ingest, Profile::Aura0).unwrap(),
    )
    .unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_aura-bench"))
        .args([
            "--operation",
            "transcode-aura0-to-aura1",
            "--dataset",
            "typed-path-json",
            "--input",
        ])
        .arg(input_path)
        .args([
            "--iterations",
            "1",
            "--warmups",
            "0",
            "--aura1-body-path",
            "columns",
            "--column-decode-path",
            "materialized",
            "--unsupported-path",
            "error",
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!("columns", json["aura1_body_path_requested"]);
    assert_eq!("columns", json["aura1_body_path"]);
    assert_eq!("materialized", json["column_decode_path_requested"]);
    assert_eq!("materialized", json["column_decode_path"]);
    assert_eq!("error", json["unsupported_path"]);
    assert!(json["execution_path_fallback_reason"].is_null());
}

#[test]
fn fair_byte_lane_cli_reports_actual_lane_dispatch() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "aura-byte-lane-dispatch-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir_all(&root).unwrap();
    let ingest = writer::encode_i64(input(
        "byte_lane_dispatch_v1",
        (0..128i64)
            .map(|index| vec![index * 1_000, index.rem_euclid(7), -index])
            .collect(),
    ))
    .unwrap();
    let aura0_path = root.join("input.aura0");
    let aura1_path = root.join("input.aura1");
    fs::write(
        &aura0_path,
        writer::compile_i64(&ingest, Profile::Aura0).unwrap(),
    )
    .unwrap();
    fs::write(
        &aura1_path,
        writer::compile_i64(&ingest, Profile::Aura1).unwrap(),
    )
    .unwrap();

    let run = |lane_use: &str, extra: &[&str]| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_aura-bench"));
        command
            .args([
                "--operation",
                "aura0-byte-lane-to-aura1-bytes",
                "--dataset",
                "lane-dispatch",
                "--input",
            ])
            .arg(&aura0_path)
            .arg("--reference-aura0")
            .arg(&aura0_path)
            .arg("--reference-aura1")
            .arg(&aura1_path)
            .args([
                "--iterations",
                "1",
                "--warmups",
                "0",
                "--aura0-profile",
                "hybrid",
                "--byte-lane-codec",
                "lz4",
                "--use-byte-lane",
                lane_use,
            ])
            .args(extra);
        command.output().unwrap()
    };

    for lane_use in ["auto", "always"] {
        let result = run(lane_use, &[]);
        assert!(
            result.status.success(),
            "{lane_use}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let json: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!("not-applicable", json["aura1_body_path_requested"]);
        assert_eq!("embedded-byte-lane", json["aura1_body_path"]);
        assert_eq!("not-applicable", json["unsupported_path"]);
        assert_eq!(true, json["byte_lane_enabled"]);
        assert_eq!("lz4", json["byte_lane_codec"]);
        assert!(json["byte_lane_compressed_bytes"].as_u64().is_some());
    }

    let never = run("never", &[]);
    assert!(
        never.status.success(),
        "{}",
        String::from_utf8_lossy(&never.stderr)
    );
    let never_json: serde_json::Value = serde_json::from_slice(&never.stdout).unwrap();
    assert_ne!("embedded-byte-lane", never_json["aura1_body_path"]);
    assert_eq!(false, never_json["byte_lane_enabled"]);
    assert!(never_json["byte_lane_codec"].is_null());
    assert!(never_json["byte_lane_compressed_bytes"].is_null());
    assert!(never_json["byte_lane_output_guard"].is_null());

    let semantic = run(
        "auto",
        &[
            "--aura1-body-path",
            "direct-streams",
            "--unsupported-path",
            "error",
        ],
    );
    assert!(
        semantic.status.success(),
        "{}",
        String::from_utf8_lossy(&semantic.stderr)
    );
    let semantic_json: serde_json::Value = serde_json::from_slice(&semantic.stdout).unwrap();
    assert_eq!("direct-streams", semantic_json["aura1_body_path"]);
    assert_eq!(false, semantic_json["byte_lane_enabled"]);
    assert!(semantic_json["byte_lane_codec"].is_null());
    assert!(semantic_json["byte_lane_compressed_bytes"].is_null());

    let semantic_verify = Command::new(env!("CARGO_BIN_EXE_aura-bench"))
        .args([
            "--operation",
            "aura0-byte-lane-to-aura1-bytes-verify",
            "--dataset",
            "lane-dispatch-verify",
            "--input",
        ])
        .arg(&aura0_path)
        .arg("--reference-aura0")
        .arg(&aura0_path)
        .arg("--reference-aura1")
        .arg(&aura1_path)
        .args([
            "--iterations",
            "1",
            "--warmups",
            "0",
            "--aura0-profile",
            "hybrid",
            "--byte-lane-codec",
            "lz4",
            "--use-byte-lane",
            "auto",
            "--aura1-body-path",
            "direct-streams",
            "--unsupported-path",
            "error",
        ])
        .output()
        .unwrap();
    assert!(
        semantic_verify.status.success(),
        "{}",
        String::from_utf8_lossy(&semantic_verify.stderr)
    );
    let verify_json: serde_json::Value = serde_json::from_slice(&semantic_verify.stdout).unwrap();
    assert_eq!("direct-streams", verify_json["aura1_body_path"]);
    assert_eq!(false, verify_json["byte_lane_enabled"]);
    assert!(verify_json["byte_lane_output_guard"].is_null());
    assert_eq!(true, verify_json["output_bytes_equal"]);

    let never_exact = run(
        "never",
        &[
            "--aura1-body-path",
            "streaming-cursor",
            "--unsupported-path",
            "error",
        ],
    );
    if never_exact.status.success() {
        let json: serde_json::Value = serde_json::from_slice(&never_exact.stdout).unwrap();
        assert_eq!("streaming-cursor", json["aura1_body_path"]);
        assert_eq!(false, json["byte_lane_enabled"]);
    } else {
        assert!(String::from_utf8_lossy(&never_exact.stderr)
            .contains("requested Aura1 body path unsupported"));
    }

    let conflict = run(
        "always",
        &[
            "--aura1-body-path",
            "direct-streams",
            "--unsupported-path",
            "error",
        ],
    );
    assert!(!conflict.status.success());
    assert!(String::from_utf8_lossy(&conflict.stderr)
        .contains("embedded byte lane conflicts with requested semantic path"));

    let pure_fast_never_fallback = Command::new(env!("CARGO_BIN_EXE_aura-bench"))
        .args([
            "--operation",
            "aura0-byte-lane-to-aura1-bytes",
            "--dataset",
            "pure-fast-never-fallback",
            "--input",
        ])
        .arg(&aura0_path)
        .arg("--reference-aura0")
        .arg(&aura0_path)
        .arg("--reference-aura1")
        .arg(&aura1_path)
        .args([
            "--iterations",
            "1",
            "--warmups",
            "0",
            "--aura0-profile",
            "fast",
            "--byte-lane-codec",
            "lz4",
            "--use-byte-lane",
            "never",
            "--aura1-body-path",
            "stable-auto",
            "--unsupported-path",
            "fallback-to-stable",
        ])
        .output()
        .unwrap();
    assert!(!pure_fast_never_fallback.status.success());
    assert!(String::from_utf8_lossy(&pure_fast_never_fallback.stderr)
        .contains("stable Aura1 fallback unsupported"));
}

#[test]
fn cli_strict_guards_never_fall_through_pure_fast_lane() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("aura-strict-guard-{}-{unique}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let ingest = writer::encode_i64(input(
        "strict_guard_cli_v1",
        (0..64i64)
            .map(|index| vec![index * 1_000, index.rem_euclid(5), -index])
            .collect(),
    ))
    .unwrap();
    let aura1 = writer::compile_i64(&ingest, Profile::Aura1).unwrap();
    let compact = writer::compile_i64(&ingest, Profile::Aura0).unwrap();
    let hybrid = records::compile_i64_file_with_aura0_profile(
        &aura1,
        records::Aura0FileProfile::Hybrid,
        records::Aura0ByteLaneCodec::Lz4,
    )
    .unwrap();
    let fast = records::compile_i64_file_with_aura0_profile(
        &aura1,
        records::Aura0FileProfile::Fast,
        records::Aura0ByteLaneCodec::Lz4,
    )
    .unwrap();
    let compact_path = root.join("compact.aura0");
    let hybrid_path = root.join("hybrid.aura0");
    let fast_path = root.join("fast.aura0");
    fs::write(&compact_path, compact).unwrap();
    fs::write(&hybrid_path, hybrid).unwrap();
    fs::write(&fast_path, fast).unwrap();

    let run = |input_path: &PathBuf, guard: &str, policy: Option<&str>| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_aura-bench"));
        command
            .args([
                "--operation",
                "transcode-aura0-to-aura1",
                "--dataset",
                "strict-guard",
                "--input",
            ])
            .arg(input_path)
            .args([
                "--iterations",
                "1",
                "--warmups",
                "0",
                "--guard-mode",
                guard,
                "--aura1-body-path",
                "stable-auto",
            ]);
        if let Some(policy) = policy {
            command.args(["--unsupported-path", policy]);
        }
        command.output().unwrap()
    };
    let run_legacy = |input_path: &PathBuf, guard: &str, transcode_path: &str, extra: &[&str]| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_aura-bench"));
        command
            .args([
                "--operation",
                "transcode-aura0-to-aura1",
                "--dataset",
                "legacy-trace",
                "--input",
            ])
            .arg(input_path)
            .args([
                "--iterations",
                "1",
                "--warmups",
                "0",
                "--guard-mode",
                guard,
                "--transcode-path",
                transcode_path,
            ])
            .args(extra);
        command.output().unwrap()
    };

    for guard in ["fused_output_guard", "block_batched_output_guard"] {
        for semantic_path in [&compact_path, &hybrid_path] {
            let legacy = run_legacy(semantic_path, guard, "auto", &[]);
            assert!(
                legacy.status.success(),
                "{}",
                String::from_utf8_lossy(&legacy.stderr)
            );
            let json: serde_json::Value = serde_json::from_slice(&legacy.stdout).unwrap();
            assert_eq!("not-applicable", json["aura1_body_path_requested"]);
            assert_eq!("direct-streams", json["aura1_body_path"]);
            assert_eq!(guard, json["guard_mode"]);
        }
        let legacy_fast = run_legacy(&fast_path, guard, "auto", &[]);
        assert!(!legacy_fast.status.success());
        assert!(String::from_utf8_lossy(&legacy_fast.stderr)
            .contains("requested strict Aura1 guard path unsupported without explicit fallback"));

        for semantic_path in [&compact_path, &hybrid_path] {
            for policy in ["error", "fallback-to-stable"] {
                let strict = run(semantic_path, guard, Some(policy));
                assert!(
                    strict.status.success(),
                    "{}",
                    String::from_utf8_lossy(&strict.stderr)
                );
                let json: serde_json::Value = serde_json::from_slice(&strict.stdout).unwrap();
                assert_eq!(guard, json["guard_mode_requested"]);
                assert_eq!(guard, json["guard_mode"]);
                assert_ne!("embedded-byte-lane", json["aura1_body_path"]);
                assert!(json["execution_path_fallback_reason"].is_null());
            }
        }

        let omitted_fast = run(&fast_path, guard, None);
        assert!(!omitted_fast.status.success());
        assert!(String::from_utf8_lossy(&omitted_fast.stderr)
            .contains("requested stable Aura1 guard/path unsupported"));

        let strict_fast = run(&fast_path, guard, Some("error"));
        assert!(!strict_fast.status.success());
        assert!(String::from_utf8_lossy(&strict_fast.stderr)
            .contains("requested stable Aura1 guard/path unsupported"));

        let fallback = run(&fast_path, guard, Some("fallback-to-stable"));
        assert!(
            fallback.status.success(),
            "{}",
            String::from_utf8_lossy(&fallback.stderr)
        );
        let json: serde_json::Value = serde_json::from_slice(&fallback.stdout).unwrap();
        assert_eq!(guard, json["guard_mode_requested"]);
        assert_eq!("old_post_output_guard", json["guard_mode"]);
        assert_eq!("embedded-byte-lane", json["aura1_body_path"]);
        assert!(json["byte_lane_enabled"].is_null());
        assert_eq!(
            "requested guard mode unsupported; used old post-output guard",
            json["execution_path_fallback_reason"]
        );
        assert!(
            json["stage_timings_ns"]["post_output_guard"]
                .as_u64()
                .unwrap()
                > 0
        );
    }

    for guard in ["no_guard", "old_post_output_guard"] {
        let compact_legacy = run_legacy(&compact_path, guard, "auto", &[]);
        assert!(compact_legacy.status.success());
        let compact_json: serde_json::Value =
            serde_json::from_slice(&compact_legacy.stdout).unwrap();
        assert_eq!("direct-streams", compact_json["aura1_body_path"]);
        assert_eq!(guard, compact_json["guard_mode"]);

        for lane_path in [&hybrid_path, &fast_path] {
            let lane = run_legacy(lane_path, guard, "auto", &[]);
            assert!(lane.status.success());
            let json: serde_json::Value = serde_json::from_slice(&lane.stdout).unwrap();
            assert_eq!("embedded-byte-lane", json["aura1_body_path"]);
            assert_eq!(guard, json["guard_mode"]);
        }
    }

    let direct = run_legacy(&compact_path, "no_guard", "direct", &[]);
    assert!(direct.status.success());
    let direct_json: serde_json::Value = serde_json::from_slice(&direct.stdout).unwrap();
    assert_eq!("direct", direct_json["transcode_path"]);
    assert_eq!("direct-streams", direct_json["aura1_body_path"]);

    let direct_fast = run_legacy(&fast_path, "no_guard", "direct", &[]);
    assert!(direct_fast.status.success());
    let direct_fast_json: serde_json::Value = serde_json::from_slice(&direct_fast.stdout).unwrap();
    assert_eq!("embedded-byte-lane", direct_fast_json["aura1_body_path"]);

    for guard in ["no_guard", "old_post_output_guard"] {
        for source in [&compact_path, &hybrid_path, &fast_path] {
            let materialized = run_legacy(source, guard, "materialized", &[]);
            assert!(
                materialized.status.success(),
                "{}",
                String::from_utf8_lossy(&materialized.stderr)
            );
            let json: serde_json::Value = serde_json::from_slice(&materialized.stdout).unwrap();
            assert_eq!("materialized", json["aura1_body_path"]);
            assert_eq!("not-applicable", json["aura1_body_path_requested"]);
            assert!(json["execution_path_fallback_reason"].is_null());
            assert_eq!(guard, json["guard_mode"]);
            assert_eq!(false, json["compiled_plan_used"]);
        }
    }
    for guard in ["fused_output_guard", "block_batched_output_guard"] {
        for source in [&compact_path, &hybrid_path, &fast_path] {
            let materialized = run_legacy(source, guard, "materialized", &[]);
            assert!(!materialized.status.success());
            assert!(String::from_utf8_lossy(&materialized.stderr).contains(
                "requested strict Aura1 guard path unsupported without explicit fallback"
            ));
        }
    }

    let cursor = run_legacy(
        &compact_path,
        "no_guard",
        "auto",
        &["--decode-path", "cursor"],
    );
    assert!(cursor.status.success());
    let cursor_json: serde_json::Value = serde_json::from_slice(&cursor.stdout).unwrap();
    assert_eq!("cursor", cursor_json["decode_path"]);
    assert_eq!("materialized-fallback", cursor_json["aura1_body_path"]);
    assert_eq!(
        "compiled execution path unavailable; materialized compiler used",
        cursor_json["execution_path_fallback_reason"]
    );

    for operation in [
        "aura0-to-aura1-bytes",
        "aura0-to-aura1-bytes-verify",
        "aura0-byte-lane-to-aura1-bytes",
        "aura0-byte-lane-to-aura1-bytes-verify",
    ] {
        for guard in ["fused_output_guard", "block_batched_output_guard"] {
            let rejected = Command::new(env!("CARGO_BIN_EXE_aura-bench"))
                .args([
                    "--operation",
                    operation,
                    "--dataset",
                    "fair-guard",
                    "--input",
                ])
                .arg(&compact_path)
                .args(["--guard-mode", guard])
                .output()
                .unwrap();
            assert!(!rejected.status.success());
            assert!(String::from_utf8_lossy(&rejected.stderr)
                .contains("fair byte operations require --guard-mode no_guard"));
        }
    }
}

#[test]
fn benchmark_rejects_inapplicable_or_conflicting_execution_flags() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "aura-execution-applicability-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir_all(&root).unwrap();
    let ingest = writer::encode_i64(input(
        "execution_applicability_v1",
        vec![vec![1, 2, 3], vec![2, 3, 4]],
    ))
    .unwrap();
    let aura1_path = root.join("input.aura1");
    fs::write(
        &aura1_path,
        writer::compile_i64(&ingest, Profile::Aura1).unwrap(),
    )
    .unwrap();

    for operation in [
        "parse-aura1",
        "aura1-replay-callback",
        "zstd-decompress-only",
        "transcode-aura1-to-aura0",
    ] {
        let result = Command::new(env!("CARGO_BIN_EXE_aura-bench"))
            .args([
                "--operation",
                operation,
                "--dataset",
                "inapplicable",
                "--input",
            ])
            .arg(&aura1_path)
            .args(["--aura1-body-path", "stable-auto"])
            .output()
            .unwrap();
        assert!(
            !result.status.success(),
            "{operation} accepted an inapplicable flag"
        );
        assert!(String::from_utf8_lossy(&result.stderr)
            .contains("typed Aura1 execution flags require an Aura0-to-Aura1 operation"));
    }

    let aura0_path = root.join("input.aura0");
    fs::write(
        &aura0_path,
        writer::compile_i64(&ingest, Profile::Aura0).unwrap(),
    )
    .unwrap();
    let conflict = Command::new(env!("CARGO_BIN_EXE_aura-bench"))
        .args([
            "--operation",
            "transcode-aura0-to-aura1",
            "--dataset",
            "conflict",
            "--input",
        ])
        .arg(&aura0_path)
        .args([
            "--decode-path",
            "materialized",
            "--aura1-body-path",
            "stable-auto",
        ])
        .output()
        .unwrap();
    assert!(!conflict.status.success());
    assert!(String::from_utf8_lossy(&conflict.stderr)
        .contains("typed Aura1 execution flags conflict with legacy --decode-path"));

    let materialized = Command::new(env!("CARGO_BIN_EXE_aura-bench"))
        .args([
            "--operation",
            "transcode-aura0-to-aura1",
            "--dataset",
            "materialized",
            "--input",
        ])
        .arg(&aura0_path)
        .args([
            "--iterations",
            "1",
            "--warmups",
            "0",
            "--transcode-path",
            "materialized",
        ])
        .output()
        .unwrap();
    assert!(
        materialized.status.success(),
        "{}",
        String::from_utf8_lossy(&materialized.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&materialized.stdout).unwrap();
    assert_eq!("materialized", json["transcode_path"]);
    assert_eq!("not-applicable", json["aura1_body_path_requested"]);
    assert_eq!("materialized", json["aura1_body_path"]);
    assert!(json["execution_path_fallback_reason"].is_null());
    assert_eq!(false, json["compiled_plan_used"]);

    let materialized_conflict = Command::new(env!("CARGO_BIN_EXE_aura-bench"))
        .args([
            "--operation",
            "transcode-aura0-to-aura1",
            "--dataset",
            "materialized-conflict",
            "--input",
        ])
        .arg(&aura0_path)
        .args([
            "--transcode-path",
            "materialized",
            "--aura1-body-path",
            "stable-auto",
        ])
        .output()
        .unwrap();
    assert!(!materialized_conflict.status.success());
    assert!(String::from_utf8_lossy(&materialized_conflict.stderr)
        .contains("typed Aura1 execution flags conflict with --transcode-path materialized"));

    let applicable_json = Command::new(env!("CARGO_BIN_EXE_aura-bench"))
        .args([
            "--operation",
            "parse-aura1",
            "--dataset",
            "not-applicable-json",
            "--input",
        ])
        .arg(&aura1_path)
        .args(["--iterations", "1", "--warmups", "0"])
        .output()
        .unwrap();
    assert!(applicable_json.status.success());
    let json: serde_json::Value = serde_json::from_slice(&applicable_json.stdout).unwrap();
    assert_eq!("not-applicable", json["aura1_body_path_requested"]);
    assert_eq!("not-applicable", json["aura1_body_path"]);
    assert_eq!("not-applicable", json["column_decode_path"]);
    assert_eq!("not-applicable", json["unsupported_path"]);
}

#[test]
fn benchmark_attributes_generic_materialized_fallback() {
    let schema = SchemaBuilder::new("execution-explicit-events")
        .field("ts", FieldType::TimestampNs, FieldRole::Timestamp)
        .field("sequence", FieldType::I64, FieldRole::Sequence)
        .repeated_field("price", FieldType::I64, FieldRole::Price)
        .repeated_field("quantity", FieldType::I64, FieldRole::Quantity)
        .finish()
        .unwrap();
    let mut event_writer = AuraI64EventWriter::new(schema);
    event_writer
        .push_event(I64Event {
            event_values: vec![1_000, 7],
            children: vec![vec![100, 2], vec![101, 3]],
        })
        .unwrap();
    let aura0 = event_writer.finish_profile(Profile::Aura0).unwrap();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let input_path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "execution-materialized-{}-{unique}.aura0",
        std::process::id()
    ));
    fs::write(&input_path, aura0).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_aura-bench"))
        .args([
            "--operation",
            "transcode-aura0-to-aura1",
            "--dataset",
            "materialized-attribution",
            "--input",
        ])
        .arg(input_path)
        .args(["--iterations", "1", "--warmups", "0"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!("not-applicable", json["aura1_body_path_requested"]);
    assert_eq!("materialized-fallback", json["aura1_body_path"]);
    assert_eq!("not-applicable", json["column_decode_path"]);
    assert_eq!(
        "compiled execution path unavailable; materialized compiler used",
        json["execution_path_fallback_reason"]
    );
}
