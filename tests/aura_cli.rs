use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use aura_codec::canonicalize_schema_json;

const MINIMAL: &str = r#"{
  "schema_format": "aura-schema",
  "schema_version": 1,
  "schema_encoding": "v3",
  "name": "cli_schema",
  "fields": [
    {
      "id": 0,
      "name": "ts",
      "type": "timestamp_ns",
      "role": "timestamp",
      "scale": 0,
      "scope": "event",
      "nullable": false,
      "relation": {"kind": "none"},
      "transform_candidates": ["absolute", "delta_base", "delta_previous", "fixed_step", "rough_step"]
    },
    {
      "id": 1,
      "name": "value",
      "type": "i64",
      "role": "value",
      "scale": 0,
      "scope": "event",
      "nullable": false,
      "relation": {"kind": "none"},
      "transform_candidates": ["absolute", "delta_base", "delta_previous", "delta2", "midpoint", "zigzag_varint", "bitpack"]
    }
  ],
  "groups": [],
  "derived_expressions": []
}"#;

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let suffix = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("aura-cli-test-{}-{suffix}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ignored = fs::remove_dir_all(&self.0);
    }
}

fn aura(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aura"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn help_and_future_subcommands_are_clear() {
    let help = aura(&["--help"]);
    assert!(help.status.success());
    let stdout = String::from_utf8(help.stdout).unwrap();
    assert!(stdout.contains("aura schema validate"));
    assert!(stdout.contains("aura schema canonicalize"));

    let nested_help = aura(&["schema", "validate", "--help"]);
    assert!(nested_help.status.success());

    let unsupported = aura(&["encode"]);
    assert!(!unsupported.status.success());
    assert!(String::from_utf8(unsupported.stderr)
        .unwrap()
        .contains("unsupported"));
}

#[test]
fn schema_validate_supports_text_and_json_results() {
    let dir = TestDir::new();
    let input = dir.path("schema.json");
    fs::write(&input, MINIMAL).unwrap();

    let text = aura(&["schema", "validate", "--input", input.to_str().unwrap()]);
    assert!(text.status.success());
    assert!(String::from_utf8(text.stdout)
        .unwrap()
        .contains("valid Aura schema \"cli_schema\""));

    let json_output = aura(&[
        "schema",
        "validate",
        "--input",
        input.to_str().unwrap(),
        "--json",
    ]);
    assert!(json_output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&json_output.stdout).unwrap();
    assert_eq!(value["valid"], true);
    assert_eq!(value["name"], "cli_schema");
    assert_eq!(value["field_count"], 2);
    assert_eq!(value["group_count"], 0);
    assert!(value["schema_id"].as_u64().is_some());
}

#[test]
fn canonicalize_writes_exact_stdout_or_explicit_file() {
    let dir = TestDir::new();
    let input = dir.path("schema.json");
    let output = dir.path("canonical.json");
    fs::write(&input, format!("\n {MINIMAL} \n")).unwrap();
    let expected = canonicalize_schema_json(MINIMAL).unwrap();

    let stdout = aura(&["schema", "canonicalize", "--input", input.to_str().unwrap()]);
    assert!(stdout.status.success());
    assert_eq!(stdout.stdout, expected.as_bytes());

    fs::write(&output, "replace me").unwrap();
    let file = aura(&[
        "schema",
        "canonicalize",
        "--input",
        input.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
    ]);
    assert!(file.status.success());
    assert!(file.stdout.is_empty());
    assert_eq!(fs::read_to_string(output).unwrap(), expected);
    assert_no_temp_files(&dir);
}

#[test]
fn canonicalize_is_safe_when_input_and_output_are_the_same_file() {
    let dir = TestDir::new();
    let path = dir.path("same.json");
    fs::write(&path, MINIMAL).unwrap();
    let expected = canonicalize_schema_json(MINIMAL).unwrap();
    let result = aura(&[
        "schema",
        "canonicalize",
        "--input",
        path.to_str().unwrap(),
        "--output",
        path.to_str().unwrap(),
    ]);
    assert!(result.status.success());
    assert_eq!(fs::read_to_string(path).unwrap(), expected);
    assert_no_temp_files(&dir);
}

#[test]
fn validation_errors_are_nonzero_and_do_not_overwrite_output() {
    let dir = TestDir::new();
    let input = dir.path("invalid.json");
    let output = dir.path("existing.json");
    fs::write(&input, r#"{"schema_format":"not-aura"}"#).unwrap();
    fs::write(&output, "preserve me").unwrap();

    let failed = aura(&[
        "schema",
        "canonicalize",
        "--input",
        input.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
    ]);
    assert!(!failed.status.success());
    assert!(!failed.stderr.is_empty());
    assert_eq!(fs::read_to_string(output).unwrap(), "preserve me");
    assert_no_temp_files(&dir);

    let json_error = aura(&[
        "schema",
        "validate",
        "--input",
        input.to_str().unwrap(),
        "--json",
    ]);
    assert!(!json_error.status.success());
    let error: serde_json::Value = serde_json::from_slice(&json_error.stderr).unwrap();
    assert_eq!(error["valid"], false);
    assert!(error["error"].as_str().is_some());
}

#[cfg(unix)]
#[test]
fn canonicalize_rejects_output_symlinks_without_touching_target_or_temp() {
    use std::os::unix::fs::symlink;

    let dir = TestDir::new();
    let input = dir.path("schema.json");
    let target = dir.path("target.json");
    let output = dir.path("output.json");
    fs::write(&input, MINIMAL).unwrap();
    fs::write(&target, "target stays").unwrap();
    symlink(&target, &output).unwrap();
    let failed = aura(&[
        "schema",
        "canonicalize",
        "--input",
        input.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
    ]);
    assert!(!failed.status.success());
    assert!(fs::symlink_metadata(&output)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(fs::read_to_string(target).unwrap(), "target stays");
    assert_no_temp_files(&dir);
}

#[test]
fn validation_text_escapes_untrusted_schema_names() {
    let dir = TestDir::new();
    let input = dir.path("escaped.json");
    fs::write(&input, MINIMAL.replace("cli_schema", "line\\nbreak")).unwrap();
    let result = aura(&["schema", "validate", "--input", input.to_str().unwrap()]);
    assert!(result.status.success());
    let stdout = String::from_utf8(result.stdout).unwrap();
    assert!(stdout.contains("\"line\\nbreak\""));
    assert_eq!(1, stdout.lines().count());
}

#[test]
fn schema_input_must_be_a_regular_file() {
    let dir = TestDir::new();
    let failed = aura(&["schema", "validate", "--input", dir.0.to_str().unwrap()]);
    assert!(!failed.status.success());
    assert!(String::from_utf8(failed.stderr)
        .unwrap()
        .contains("regular file"));
}

fn assert_no_temp_files(dir: &TestDir) {
    let names = fs::read_dir(&dir.0)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(names.iter().all(|name| !name.contains(".aura-tmp-")));
}
