use std::env;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};

use aura_codec::{parse_schema_json, MAX_SCHEMA_JSON_BYTES};

const HELP: &str = "Aura developer CLI

Usage:
  aura schema validate --input <path> [--json]
  aura schema canonicalize --input <path> [--output <path>]
  aura --help
";

static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
struct CliError(String);

fn main() -> ExitCode {
    let args = env::args().skip(1).collect::<Vec<_>>();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if args.iter().any(|arg| arg == "--json") {
                let message = serde_json::to_string(&error.0)
                    .unwrap_or_else(|_| "\"schema command failed\"".to_owned());
                eprintln!("{{\"valid\":false,\"error\":{message}}}");
            } else {
                eprintln!("error: {}", error.0);
            }
            ExitCode::from(2)
        }
    }
}

fn run(args: &[String]) -> Result<(), CliError> {
    if matches!(args, [arg] if arg == "--help" || arg == "-h") {
        print!("{HELP}");
        return Ok(());
    }
    if let Some(namespace) = args.first() {
        if namespace != "schema" {
            return Err(CliError(format!(
                "unsupported command {namespace:?}; only schema is available"
            )));
        }
    }
    let [namespace, command, options @ ..] = args else {
        return Err(CliError(format!("invalid command\n\n{HELP}")));
    };
    debug_assert_eq!(namespace, "schema");
    if command == "--help" || command == "-h" {
        print!("{HELP}");
        return Ok(());
    }
    match command.as_str() {
        "validate" => validate_command(options),
        "canonicalize" => canonicalize_command(options),
        _ => Err(CliError(format!(
            "unsupported schema command {command:?}; expected validate or canonicalize"
        ))),
    }
}

fn validate_command(args: &[String]) -> Result<(), CliError> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print!("{HELP}");
        return Ok(());
    }
    let mut input = None;
    let mut json_output = false;
    parse_options(args, |name, value| match name {
        "--input" => set_path(&mut input, value, "--input"),
        "--json" if value.is_none() => {
            json_output = true;
            Ok(())
        }
        _ => Err(CliError(format!("unknown validate option {name:?}"))),
    })?;
    let input = input.ok_or_else(|| CliError("missing required --input <path>".to_owned()))?;
    let text = read_bounded_schema(&input)?;
    let schema = parse_schema_json(&text).map_err(|error| CliError(error.to_string()))?;
    if json_output {
        let name = serde_json::to_string(&schema.name)
            .map_err(|_| CliError("could not serialize validation result".to_owned()))?;
        println!(
            "{{\"valid\":true,\"schema_id\":{},\"name\":{},\"field_count\":{},\"group_count\":{}}}",
            schema.schema_id,
            name,
            schema.fields.len(),
            schema.groups.len()
        );
    } else {
        let escaped_name = serde_json::to_string(&schema.name)
            .map_err(|_| CliError("could not serialize schema name".to_owned()))?;
        println!("valid Aura schema {escaped_name} ({})", schema.schema_id);
    }
    Ok(())
}

fn canonicalize_command(args: &[String]) -> Result<(), CliError> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print!("{HELP}");
        return Ok(());
    }
    let mut input = None;
    let mut output = None;
    parse_options(args, |name, value| match name {
        "--input" => set_path(&mut input, value, "--input"),
        "--output" => set_path(&mut output, value, "--output"),
        _ => Err(CliError(format!("unknown canonicalize option {name:?}"))),
    })?;
    let input = input.ok_or_else(|| CliError("missing required --input <path>".to_owned()))?;
    let text = read_bounded_schema(&input)?;
    let schema = parse_schema_json(&text).map_err(|error| CliError(error.to_string()))?;
    let canonical = schema
        .to_canonical_json()
        .map_err(|error| CliError(error.to_string()))?;
    if let Some(output) = output {
        write_schema_atomically(&output, canonical.as_bytes())?;
    } else {
        io::stdout()
            .lock()
            .write_all(canonical.as_bytes())
            .map_err(|error| CliError(format!("could not write stdout: {error}")))?;
    }
    Ok(())
}

fn write_schema_atomically(output: &Path, bytes: &[u8]) -> Result<(), CliError> {
    match fs::symlink_metadata(output) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(CliError(format!(
                "schema output is not a regular file: {}",
                output.display()
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(CliError(format!(
                "could not inspect output {}: {error}",
                output.display()
            )));
        }
    }

    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    let file_name = output
        .file_name()
        .ok_or_else(|| CliError("output path has no file name".to_owned()))?;
    for _ in 0..128 {
        let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
        let mut temp_name = OsString::from(".");
        temp_name.push(file_name);
        temp_name.push(format!(".aura-tmp-{}-{sequence}", std::process::id()));
        let temp_path = parent.join(temp_name);
        let mut temp = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(CliError(format!(
                    "could not create temporary output beside {}: {error}",
                    output.display()
                )));
            }
        };

        let write_result = temp
            .write_all(bytes)
            .and_then(|()| temp.flush())
            .and_then(|()| temp.sync_all());
        drop(temp);
        if let Err(error) = write_result {
            let _ignored = fs::remove_file(&temp_path);
            return Err(CliError(format!(
                "could not write output {}: {error}",
                output.display()
            )));
        }
        if let Err(error) = fs::rename(&temp_path, output) {
            let _ignored = fs::remove_file(&temp_path);
            return Err(CliError(format!(
                "could not replace output {}: {error}",
                output.display()
            )));
        }
        return Ok(());
    }
    Err(CliError(format!(
        "could not create a unique temporary output beside {}",
        output.display()
    )))
}

fn parse_options(
    args: &[String],
    mut option: impl FnMut(&str, Option<&str>) -> Result<(), CliError>,
) -> Result<(), CliError> {
    let mut index = 0;
    while index < args.len() {
        let name = args[index].as_str();
        if name == "--json" {
            option(name, None)?;
            index += 1;
            continue;
        }
        let value = args
            .get(index + 1)
            .ok_or_else(|| CliError(format!("missing value for {name}")))?;
        if value.starts_with("--") {
            return Err(CliError(format!("missing value for {name}")));
        }
        option(name, Some(value))?;
        index += 2;
    }
    Ok(())
}

fn set_path(slot: &mut Option<PathBuf>, value: Option<&str>, option: &str) -> Result<(), CliError> {
    if slot.is_some() {
        return Err(CliError(format!("duplicate option {option}")));
    }
    let value = value.ok_or_else(|| CliError(format!("missing value for {option}")))?;
    *slot = Some(PathBuf::from(value));
    Ok(())
}

fn read_bounded_schema(path: &Path) -> Result<String, CliError> {
    let metadata = fs::metadata(path).map_err(|error| {
        CliError(format!(
            "could not inspect input {}: {error}",
            path.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(CliError(format!(
            "schema input is not a regular file: {}",
            path.display()
        )));
    }
    if metadata.len() > MAX_SCHEMA_JSON_BYTES as u64 {
        return Err(CliError(format!(
            "schema input exceeds {MAX_SCHEMA_JSON_BYTES} bytes"
        )));
    }
    let file = File::open(path)
        .map_err(|error| CliError(format!("could not open input {}: {error}", path.display())))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(metadata.len() as usize)
        .map_err(|_| CliError("could not allocate schema input buffer".to_owned()))?;
    file.take((MAX_SCHEMA_JSON_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| CliError(format!("could not read input {}: {error}", path.display())))?;
    if bytes.len() > MAX_SCHEMA_JSON_BYTES {
        return Err(CliError(format!(
            "schema input exceeds {MAX_SCHEMA_JSON_BYTES} bytes"
        )));
    }
    String::from_utf8(bytes).map_err(|_| CliError("schema input is not UTF-8".to_owned()))
}
