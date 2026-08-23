use std::env;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::io::{Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};

use aura_codec::{
    arrow_rust_version, build_provenance, canonical_v3_batch_sha256, cargo_lock_sha256,
    decode_v3_value_block, encode_shadow_arrow_ipc, parse_schema_json, ShadowEncodeResult,
    ShadowProtocolLimits, V3ValueLimits, MAX_SCHEMA_JSON_BYTES, MAX_V3_VALUE_BLOCK_BYTES,
    SHADOW_ARROW_PROTOCOL, SHADOW_ARTIFACT_KIND, SHADOW_HANDSHAKE_SCHEMA, SHADOW_PROTOCOL,
    SHADOW_RESULT_SCHEMA, SHADOW_SCHEMA_FORMAT, SHADOW_VERIFY_RESULT_SCHEMA,
};
use serde_json::json;
use sha2::{Digest, Sha256};

const HELP: &str = "Aura developer CLI

Usage:
  aura schema validate --input <path> [--json]
  aura schema canonicalize --input <path> [--output <path>]
  aura shadow handshake --protocol aura-logical-arrow-ipc-v1 --json
  aura shadow encode --protocol aura-logical-arrow-ipc-v1 --schema <path> \
    --artifact-kind standalone-aura-v3-value-block-v1 --output <path> --json
  aura shadow verify --protocol aura-logical-arrow-ipc-v1 --schema <path> \
    --input <existing.aurav3vb> --json
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
            let shadow_error = args.first().is_some_and(|value| value == "shadow");
            let (error_code, error_text) = if shadow_error {
                shadow_safe_error(&error.0)
            } else {
                ("schema_command_failed", bounded_error(&error.0))
            };
            if args.iter().any(|arg| arg == "--json") {
                let message = serde_json::to_string(&error_text)
                    .unwrap_or_else(|_| "\"schema command failed\"".to_owned());
                if shadow_error {
                    eprintln!("{{\"error_schema\":\"aura-shadow-error-v1\",\"code\":\"{error_code}\",\"error\":{message}}}");
                } else {
                    eprintln!("{{\"valid\":false,\"error\":{message}}}");
                }
            } else {
                eprintln!("error: {error_text}");
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
        if namespace != "schema" && namespace != "shadow" {
            return Err(CliError(format!(
                "unsupported command {namespace:?}; expected schema or shadow"
            )));
        }
    }
    let [namespace, command, options @ ..] = args else {
        return Err(CliError(format!("invalid command\n\n{HELP}")));
    };
    if command == "--help" || command == "-h" {
        print!("{HELP}");
        return Ok(());
    }
    match (namespace.as_str(), command.as_str()) {
        ("schema", "validate") => validate_command(options),
        ("schema", "canonicalize") => canonicalize_command(options),
        ("shadow", "handshake") => shadow_handshake_command(options),
        ("shadow", "encode") => shadow_encode_command(options),
        ("shadow", "verify") => shadow_verify_command(options),
        ("schema", _) => Err(CliError(format!(
            "unsupported schema command {command:?}; expected validate or canonicalize"
        ))),
        ("shadow", _) => Err(CliError(format!(
            "unsupported shadow command {command:?}; expected handshake, encode, or verify"
        ))),
        _ => unreachable!(),
    }
}

fn shadow_handshake_command(args: &[String]) -> Result<(), CliError> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print!("{HELP}");
        return Ok(());
    }
    let mut protocol = None;
    let mut json_output = false;
    parse_options(args, |name, value| match name {
        "--protocol" => set_string(&mut protocol, value, "--protocol"),
        "--json" if value.is_none() => {
            json_output = true;
            Ok(())
        }
        _ => Err(CliError("unknown shadow handshake option".to_owned())),
    })?;
    require_protocol(protocol)?;
    if !json_output {
        return Err(CliError("shadow handshake requires --json".to_owned()));
    }
    let provenance = build_provenance();
    let value = json!({
        "handshake_schema": SHADOW_HANDSHAKE_SCHEMA,
        "package": env!("CARGO_PKG_NAME"),
        "package_version": env!("CARGO_PKG_VERSION"),
        "git_commit": provenance.git_commit,
        "dirty": provenance.dirty,
        "provenance_source": provenance.source,
        "protocols": [SHADOW_PROTOCOL],
        "schema_formats": [SHADOW_SCHEMA_FORMAT],
        "artifact_kinds": [SHADOW_ARTIFACT_KIND],
        "operations": ["encode", "verify"],
        "complete_container_targets": [],
        "hash_contracts": {
            "schema_fingerprint": "sha256:aura-v3-schema-fingerprint-v1",
            "logical_values": "sha256:aura-v3-canonical-exact-values-v1",
            "artifact": "sha256"
        },
        "arrow": {
            "rust_version": arrow_rust_version(),
            "protocol": SHADOW_ARROW_PROTOCOL
        },
        "arrow_crate_version": arrow_rust_version(),
        "cargo_lock_sha256": hex(&cargo_lock_sha256())
    });
    write_json_stdout(&value)
}

fn shadow_encode_command(args: &[String]) -> Result<(), CliError> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print!("{HELP}");
        return Ok(());
    }
    let mut protocol = None;
    let mut schema_path = None;
    let mut artifact_kind = None;
    let mut output = None;
    let mut json_output = false;
    parse_options(args, |name, value| match name {
        "--protocol" => set_string(&mut protocol, value, "--protocol"),
        "--schema" => set_path(&mut schema_path, value, "--schema"),
        "--artifact-kind" => set_string(&mut artifact_kind, value, "--artifact-kind"),
        "--output" => set_path(&mut output, value, "--output"),
        "--json" if value.is_none() => {
            json_output = true;
            Ok(())
        }
        _ => Err(CliError("unknown shadow encode option".to_owned())),
    })?;
    require_protocol(protocol)?;
    if artifact_kind.as_deref() != Some(SHADOW_ARTIFACT_KIND) {
        return Err(CliError(
            "unsupported or missing --artifact-kind".to_owned(),
        ));
    }
    if !json_output {
        return Err(CliError("shadow encode requires --json".to_owned()));
    }
    let schema_path = schema_path.ok_or_else(|| CliError("missing --schema".to_owned()))?;
    let output = output.ok_or_else(|| CliError("missing --output".to_owned()))?;
    if schema_path == Path::new("-") || output == Path::new("-") {
        return Err(CliError(
            "shadow schema and output must be filesystem paths".to_owned(),
        ));
    }
    if output.extension().and_then(|value| value.to_str()) != Some("aurav3vb") {
        return Err(CliError(
            "shadow output must use the .aurav3vb extension".to_owned(),
        ));
    }
    require_absent_output(&output)?;
    let schema_text = read_bounded_schema(&schema_path)?;
    let schema = parse_schema_json(&schema_text).map_err(|error| CliError(error.to_string()))?;
    let result =
        encode_shadow_arrow_ipc(&schema, io::stdin().lock(), ShadowProtocolLimits::default())
            .map_err(|error| CliError(error.to_string()))?;
    let publication = publish_verified_block(&output, &schema, &result)?;

    let provenance = build_provenance();
    let value = json!({
        "result_schema": SHADOW_RESULT_SCHEMA,
        "protocol": SHADOW_PROTOCOL,
        "artifact_kind": SHADOW_ARTIFACT_KIND,
        "complete_aura_file": false,
        "reference_block_version": 1,
        "package_version": env!("CARGO_PKG_VERSION"),
        "schema_id": schema.schema_id,
        "schema_fingerprint_sha256": hex(&result.schema_fingerprint),
        "row_count": result.row_count,
        "logical_sha256": hex(&result.logical_sha256),
        "artifact_bytes": result.block.len(),
        "artifact_sha256": hex(&result.block_sha256),
        "stale_temp_cleanup_required": publication.stale_temp_cleanup_required,
        "build": {
            "git_commit": provenance.git_commit,
            "dirty": provenance.dirty,
            "provenance_source": provenance.source,
            "arrow_crate_version": arrow_rust_version(),
            "cargo_lock_sha256": hex(&cargo_lock_sha256())
        }
    });
    write_json_stdout(&value)
        .map_err(|_| CliError("publication committed result unavailable".to_owned()))
}

fn shadow_verify_command(args: &[String]) -> Result<(), CliError> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print!("{HELP}");
        return Ok(());
    }
    let mut protocol = None;
    let mut schema_path = None;
    let mut input = None;
    let mut json_output = false;
    parse_options(args, |name, value| match name {
        "--protocol" => set_string(&mut protocol, value, "--protocol"),
        "--schema" => set_path(&mut schema_path, value, "--schema"),
        "--input" => set_path(&mut input, value, "--input"),
        "--json" if value.is_none() => {
            json_output = true;
            Ok(())
        }
        _ => Err(CliError("unknown shadow verify option".to_owned())),
    })?;
    require_protocol(protocol)?;
    if !json_output {
        return Err(CliError("shadow verify requires --json".to_owned()));
    }
    let schema_path = schema_path.ok_or_else(|| CliError("missing --schema".to_owned()))?;
    let input = input.ok_or_else(|| CliError("missing --input".to_owned()))?;
    if schema_path == Path::new("-") || input == Path::new("-") {
        return Err(CliError(
            "shadow schema and input must be filesystem paths".to_owned(),
        ));
    }
    if input.extension().and_then(|value| value.to_str()) != Some("aurav3vb") {
        return Err(CliError(
            "shadow input must use the .aurav3vb extension".to_owned(),
        ));
    }
    let schema_text = read_bounded_schema(&schema_path)?;
    let schema = parse_schema_json(&schema_text).map_err(|error| CliError(error.to_string()))?;
    let verify_limits = ShadowProtocolLimits::default().values;
    let bytes = read_bounded_reference_block(&input, verify_limits.max_block_bytes)?;
    let decoded = decode_v3_value_block(&schema, &bytes, verify_limits)
        .map_err(|_| CliError("invalid shadow reference block".to_owned()))?;
    let schema_fingerprint = aura_codec::canonical_v3_schema_fingerprint(&schema)
        .map_err(|_| CliError("invalid shadow schema fingerprint".to_owned()))?;
    let logical_sha256 = canonical_v3_batch_sha256(&schema, &decoded, verify_limits)
        .map_err(|_| CliError("invalid shadow reference block".to_owned()))?;
    let artifact_sha256: [u8; 32] = Sha256::digest(&bytes).into();
    let provenance = build_provenance();
    let value = json!({
        "result_schema": SHADOW_VERIFY_RESULT_SCHEMA,
        "protocol": SHADOW_PROTOCOL,
        "artifact_kind": SHADOW_ARTIFACT_KIND,
        "complete_aura_file": false,
        "reference_block_version": 1,
        "package_version": env!("CARGO_PKG_VERSION"),
        "schema_id": schema.schema_id,
        "schema_fingerprint_sha256": hex(&schema_fingerprint),
        "row_count": decoded.row_count,
        "logical_sha256": hex(&logical_sha256),
        "artifact_bytes": bytes.len(),
        "artifact_sha256": hex(&artifact_sha256),
        "build": {
            "git_commit": provenance.git_commit,
            "dirty": provenance.dirty,
            "provenance_source": provenance.source,
            "arrow_crate_version": arrow_rust_version(),
            "cargo_lock_sha256": hex(&cargo_lock_sha256())
        }
    });
    write_json_stdout(&value)
}

fn require_protocol(protocol: Option<String>) -> Result<(), CliError> {
    if protocol.as_deref() == Some(SHADOW_PROTOCOL) {
        Ok(())
    } else {
        Err(CliError("unsupported or missing --protocol".to_owned()))
    }
}

fn write_json_stdout(value: &serde_json::Value) -> Result<(), CliError> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, value)
        .map_err(|_| CliError("could not serialize shadow result".to_owned()))?;
    stdout
        .write_all(b"\n")
        .map_err(|_| CliError("could not write stdout".to_owned()))
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

    let parent = normalized_parent(output);
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

fn require_absent_output(output: &Path) -> Result<(), CliError> {
    match fs::symlink_metadata(output) {
        Ok(_) => Err(CliError(
            "shadow output already exists or is a symlink".to_owned(),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(CliError("could not inspect shadow output".to_owned())),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PublicationOutcome {
    stale_temp_cleanup_required: bool,
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    length: u64,
}

#[cfg(unix)]
trait PublicationOps {
    fn open_parent(&self, path: &Path) -> io::Result<File> {
        File::open(path)
    }

    fn create_temp(&self, path: &Path) -> io::Result<File> {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        if file.metadata()?.permissions().mode() & 0o777 != 0o600 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "shadow temporary output mode",
            ));
        }
        Ok(file)
    }

    fn symlink_metadata(&self, path: &Path) -> io::Result<fs::Metadata> {
        fs::symlink_metadata(path)
    }

    fn hard_link(&self, source: &Path, destination: &Path) -> io::Result<()> {
        fs::hard_link(source, destination)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
    }

    fn sync_parent(&self, directory: &File) -> io::Result<()> {
        directory.sync_all()
    }

    fn before_link(&self, _temp: &Path, _output: &Path) -> io::Result<()> {
        Ok(())
    }

    fn after_link(&self, _temp: &Path, _output: &Path) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(unix)]
struct RealPublicationOps;

#[cfg(unix)]
impl PublicationOps for RealPublicationOps {}

fn publish_verified_block(
    output: &Path,
    schema: &aura_codec::SchemaDescriptor,
    result: &ShadowEncodeResult,
) -> Result<PublicationOutcome, CliError> {
    #[cfg(not(unix))]
    {
        let _ = (output, schema, result);
        return Err(CliError(
            "trusted shadow publication is unsupported on this platform".to_owned(),
        ));
    }
    #[cfg(unix)]
    {
        publish_verified_block_with_ops(output, schema, result, &RealPublicationOps)
    }
}

#[cfg(unix)]
fn publish_verified_block_with_ops(
    output: &Path,
    schema: &aura_codec::SchemaDescriptor,
    result: &ShadowEncodeResult,
    ops: &impl PublicationOps,
) -> Result<PublicationOutcome, CliError> {
    let parent = normalized_parent(output);
    let directory = ops
        .open_parent(parent)
        .map_err(|_| CliError("could not open trusted shadow output directory".to_owned()))?;
    validate_trusted_parent(parent, &directory, ops)?;
    let file_name = output
        .file_name()
        .ok_or_else(|| CliError("shadow output has no file name".to_owned()))?;
    for _ in 0..128 {
        let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
        let mut temp_name = OsString::from(".");
        temp_name.push(file_name);
        temp_name.push(format!(".aura-tmp-{}-{sequence}", std::process::id()));
        let temp_path = parent.join(temp_name);
        let mut temp = match ops.create_temp(&temp_path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(_) => {
                return Err(CliError(
                    "could not create shadow temporary output".to_owned(),
                ))
            }
        };
        let write_result = temp
            .write_all(&result.block)
            .and_then(|()| temp.flush())
            .and_then(|()| temp.sync_all());
        if write_result.is_err() {
            let identity = temp
                .metadata()
                .ok()
                .map(|metadata| file_identity(&metadata));
            return Err(prelink_failure(
                &directory,
                &temp_path,
                identity,
                ops,
                CliError("could not write shadow temporary output".to_owned()),
            ));
        }
        let held_identity =
            file_identity(&temp.metadata().map_err(|_| {
                CliError("could not inspect held shadow temporary output".to_owned())
            })?);
        if let Err(error) = verify_temp_block(&mut temp, held_identity, schema, result) {
            return Err(prelink_failure(
                &directory,
                &temp_path,
                Some(held_identity),
                ops,
                error,
            ));
        }
        if ops.before_link(&temp_path, output).is_err()
            || !path_matches_identity(&temp_path, held_identity, ops)
        {
            return Err(prelink_failure(
                &directory,
                &temp_path,
                Some(held_identity),
                ops,
                CliError("shadow temporary output identity changed before publication".to_owned()),
            ));
        }
        if let Err(error) = validate_trusted_parent(parent, &directory, ops) {
            return Err(prelink_failure(
                &directory,
                &temp_path,
                Some(held_identity),
                ops,
                error,
            ));
        }
        if ops.hard_link(&temp_path, output).is_err() {
            return Err(prelink_failure(
                &directory,
                &temp_path,
                Some(held_identity),
                ops,
                CliError("could not publish shadow output without overwrite".to_owned()),
            ));
        }
        let identity_ok = ops.after_link(&temp_path, output).is_ok()
            && path_matches_identity(&temp_path, held_identity, ops)
            && path_matches_identity(output, held_identity, ops);
        let final_verified =
            identity_ok && verify_temp_block(&mut temp, held_identity, schema, result).is_ok();
        if !final_verified {
            if rollback_uncommitted(&directory, &temp_path, output, held_identity, ops) {
                return Err(CliError(
                    "shadow publication identity verification failed".to_owned(),
                ));
            }
            return Err(CliError("publication ambiguous".to_owned()));
        }
        if ops.sync_parent(&directory).is_err() {
            if rollback_uncommitted(&directory, &temp_path, output, held_identity, ops) {
                return Err(CliError(
                    "could not commit shadow output directory".to_owned(),
                ));
            }
            return Err(CliError("publication ambiguous".to_owned()));
        }
        let stale_temp_cleanup_required = if remove_if_owned(&temp_path, held_identity, ops) {
            ops.sync_parent(&directory).is_err()
        } else {
            true
        };
        return Ok(PublicationOutcome {
            stale_temp_cleanup_required,
        });
    }
    Err(CliError(
        "could not create unique shadow temporary output".to_owned(),
    ))
}

#[cfg(unix)]
fn validate_trusted_parent(
    parent: &Path,
    directory: &File,
    ops: &impl PublicationOps,
) -> Result<(), CliError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let held = directory
        .metadata()
        .map_err(|_| CliError("could not inspect trusted shadow output directory".to_owned()))?;
    let path = ops
        .symlink_metadata(parent)
        .map_err(|_| CliError("could not inspect trusted shadow output directory".to_owned()))?;
    if !held.is_dir()
        || !path.is_dir()
        || path.file_type().is_symlink()
        || held.dev() != path.dev()
        || held.ino() != path.ino()
        || path.permissions().mode() & 0o022 != 0
    {
        return Err(CliError(
            "shadow output directory is not trusted".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn file_identity(metadata: &fs::Metadata) -> FileIdentity {
    use std::os::unix::fs::MetadataExt;
    FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        length: metadata.len(),
    }
}

#[cfg(unix)]
fn path_matches_identity(path: &Path, expected: FileIdentity, ops: &impl PublicationOps) -> bool {
    ops.symlink_metadata(path).is_ok_and(|metadata| {
        metadata.is_file()
            && !metadata.file_type().is_symlink()
            && file_identity(&metadata) == expected
    })
}

#[cfg(unix)]
fn cleanup_owned_path(
    path: &Path,
    identity: Option<FileIdentity>,
    ops: &impl PublicationOps,
) -> bool {
    let Some(identity) = identity else {
        return false;
    };
    let _ = remove_if_owned(path, identity, ops);
    !path_matches_identity(path, identity, ops)
}

#[cfg(unix)]
fn prelink_failure(
    directory: &File,
    temp_path: &Path,
    identity: Option<FileIdentity>,
    ops: &impl PublicationOps,
    ordinary: CliError,
) -> CliError {
    if cleanup_owned_path(temp_path, identity, ops) && ops.sync_parent(directory).is_ok() {
        ordinary
    } else {
        CliError("publication cleanup required".to_owned())
    }
}

#[cfg(unix)]
fn remove_if_owned(path: &Path, identity: FileIdentity, ops: &impl PublicationOps) -> bool {
    path_matches_identity(path, identity, ops) && ops.remove_file(path).is_ok()
}

#[cfg(unix)]
fn rollback_uncommitted(
    directory: &File,
    temp_path: &Path,
    output: &Path,
    identity: FileIdentity,
    ops: &impl PublicationOps,
) -> bool {
    let _ = remove_if_owned(output, identity, ops);
    let _ = remove_if_owned(temp_path, identity, ops);
    let final_absent = matches!(
        ops.symlink_metadata(output),
        Err(error) if error.kind() == io::ErrorKind::NotFound
    );
    let owned_temp_absent = !path_matches_identity(temp_path, identity, ops);
    final_absent && owned_temp_absent && ops.sync_parent(directory).is_ok()
}

#[cfg(unix)]
fn verify_temp_block(
    temp: &mut File,
    expected_identity: FileIdentity,
    schema: &aura_codec::SchemaDescriptor,
    expected: &ShadowEncodeResult,
) -> Result<(), CliError> {
    let metadata = temp
        .metadata()
        .map_err(|_| CliError("could not inspect shadow temporary output".to_owned()))?;
    if !metadata.is_file()
        || metadata.len() != expected.block.len() as u64
        || metadata.len() > MAX_V3_VALUE_BLOCK_BYTES as u64
        || file_identity(&metadata) != expected_identity
    {
        return Err(CliError(
            "invalid shadow temporary output length".to_owned(),
        ));
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(expected.block.len())
        .map_err(|_| CliError("could not allocate verification buffer".to_owned()))?;
    temp.seek(SeekFrom::Start(0))
        .map_err(|_| CliError("could not seek shadow temporary output".to_owned()))?;
    temp.take((MAX_V3_VALUE_BLOCK_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| CliError("could not verify shadow temporary output".to_owned()))?;
    let artifact_hash: [u8; 32] = Sha256::digest(&bytes).into();
    if artifact_hash != expected.block_sha256 {
        return Err(CliError("shadow temporary output hash mismatch".to_owned()));
    }
    let decoded = decode_v3_value_block(schema, &bytes, V3ValueLimits::default())
        .map_err(|_| CliError("shadow temporary output decode failed".to_owned()))?;
    let logical = canonical_v3_batch_sha256(schema, &decoded, V3ValueLimits::default())
        .map_err(|_| CliError("shadow temporary output logical verification failed".to_owned()))?;
    if logical != expected.logical_sha256 || decoded.row_count != expected.row_count {
        return Err(CliError(
            "shadow temporary output logical hash mismatch".to_owned(),
        ));
    }
    Ok(())
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

fn normalized_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn set_string(
    slot: &mut Option<String>,
    value: Option<&str>,
    option: &str,
) -> Result<(), CliError> {
    if slot.is_some() {
        return Err(CliError(format!("duplicate option {option}")));
    }
    let value = value.ok_or_else(|| CliError(format!("missing value for {option}")))?;
    *slot = Some(value.to_owned());
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

fn bounded_error(message: &str) -> String {
    message
        .chars()
        .take(2048)
        .map(|character| {
            if character.is_control() && character != '\t' {
                ' '
            } else {
                character
            }
        })
        .collect()
}

fn shadow_safe_error(message: &str) -> (&'static str, String) {
    let (code, safe) = if message == "publication ambiguous" {
        (
            "publication_ambiguous",
            "shadow publication state requires coordinator reconciliation",
        )
    } else if message == "publication cleanup required" {
        (
            "publication_cleanup_required",
            "no final artifact was committed; coordinator cleanup of a stale temporary file is required",
        )
    } else if message == "publication committed result unavailable" {
        (
            "publication_committed_result_unavailable",
            "the artifact was committed; coordinator must verify and adopt the final artifact",
        )
    } else if message.contains("unsupported on this platform") {
        (
            "publication_unsupported",
            "trusted shadow publication is unsupported on this platform",
        )
    } else if message.contains("not trusted") {
        (
            "untrusted_output_directory",
            "shadow output directory is not trusted",
        )
    } else if message.contains("already exists") || message.contains("symlink") {
        ("output_exists", "shadow output is not a new path")
    } else if message.contains("extension") || message.contains("filesystem paths") {
        ("invalid_output_path", "shadow output path is invalid")
    } else if message.contains("protocol") || message.contains("artifact-kind") {
        (
            "unsupported_contract",
            "shadow protocol or artifact kind is unsupported",
        )
    } else if message.contains("schema") && !message.contains("temporary output") {
        ("invalid_schema", "shadow schema is invalid or unavailable")
    } else if message.contains("reference block") {
        (
            "invalid_artifact",
            "shadow reference block is invalid or unavailable",
        )
    } else if message.contains("shadow ipc")
        || message.contains("shadow arrow")
        || message.contains("unexpected end")
    {
        ("invalid_ipc", "shadow Arrow IPC input is invalid")
    } else if message.contains("publish")
        || message.contains("temporary output")
        || message.contains("synchronize")
    {
        ("publication_failed", "shadow artifact publication failed")
    } else {
        ("shadow_command_failed", "shadow command failed")
    };
    (code, safe.to_owned())
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

fn read_bounded_reference_block(path: &Path, max_block_bytes: usize) -> Result<Vec<u8>, CliError> {
    let max_block_bytes = max_block_bytes.min(MAX_V3_VALUE_BLOCK_BYTES);
    let path_metadata = fs::symlink_metadata(path)
        .map_err(|_| CliError("could not inspect shadow reference block".to_owned()))?;
    if path_metadata.file_type().is_symlink() || !path_metadata.is_file() {
        return Err(CliError(
            "shadow reference block is not a regular non-symlink file".to_owned(),
        ));
    }
    if path_metadata.len() > max_block_bytes as u64 {
        return Err(CliError(
            "shadow reference block exceeds byte limit".to_owned(),
        ));
    }
    let file = File::open(path)
        .map_err(|_| CliError("could not open shadow reference block".to_owned()))?;
    let held_metadata = file
        .metadata()
        .map_err(|_| CliError("could not inspect shadow reference block".to_owned()))?;
    if !held_metadata.is_file() || held_metadata.len() != path_metadata.len() {
        return Err(CliError(
            "shadow reference block identity changed".to_owned(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if held_metadata.dev() != path_metadata.dev() || held_metadata.ino() != path_metadata.ino()
        {
            return Err(CliError(
                "shadow reference block identity changed".to_owned(),
            ));
        }
    }
    let length = usize::try_from(held_metadata.len())
        .map_err(|_| CliError("shadow reference block exceeds byte limit".to_owned()))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| CliError("could not allocate shadow reference block buffer".to_owned()))?;
    file.take((max_block_bytes + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| CliError("could not read shadow reference block".to_owned()))?;
    if bytes.len() != length || bytes.len() > max_block_bytes {
        return Err(CliError(
            "shadow reference block identity changed".to_owned(),
        ));
    }
    Ok(bytes)
}

#[cfg(all(test, unix))]
mod publication_tests {
    use std::cell::Cell;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    use aura_codec::{
        canonical_v3_batch_sha256, canonical_v3_schema_fingerprint, encode_v3_value_block,
        AuraV3Batch, AuraV3Column, AuraV3ColumnValues, FieldRole, FieldType, SchemaBuilder,
    };

    use super::*;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
            let path = env::temp_dir().join(format!(
                "aura-publication-unit-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }

        fn output(&self) -> PathBuf {
            self.0.join("values.aurav3vb")
        }

        fn temp_paths(&self) -> Vec<PathBuf> {
            fs::read_dir(&self.0)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| {
                    path.file_name()
                        .is_some_and(|name| name.to_string_lossy().contains(".aura-tmp-"))
                })
                .collect()
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Default)]
    struct FaultOps {
        fail_open: bool,
        replace_temp_before_link: bool,
        fail_temp_remove_after_link: bool,
        fail_temp_remove_before_link: bool,
        fail_all_remove_after_link: bool,
        fail_sync_call: Option<usize>,
        sync_calls: Cell<usize>,
        linked: Cell<bool>,
    }

    impl PublicationOps for FaultOps {
        fn open_parent(&self, path: &Path) -> io::Result<File> {
            if self.fail_open {
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "injected"))
            } else {
                File::open(path)
            }
        }

        fn create_temp(&self, path: &Path) -> io::Result<File> {
            OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
        }

        fn hard_link(&self, source: &Path, destination: &Path) -> io::Result<()> {
            let result = fs::hard_link(source, destination);
            if result.is_ok() {
                self.linked.set(true);
            }
            result
        }

        fn remove_file(&self, path: &Path) -> io::Result<()> {
            let is_temp = path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().contains(".aura-tmp-"));
            if (is_temp && self.fail_temp_remove_before_link && !self.linked.get())
                || (self.linked.get()
                    && (self.fail_all_remove_after_link
                        || (is_temp && self.fail_temp_remove_after_link)))
            {
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "injected"))
            } else {
                fs::remove_file(path)
            }
        }

        fn sync_parent(&self, directory: &File) -> io::Result<()> {
            let call = self.sync_calls.get() + 1;
            self.sync_calls.set(call);
            if self.fail_sync_call == Some(call) {
                Err(io::Error::other("injected"))
            } else {
                directory.sync_all()
            }
        }

        fn before_link(&self, temp: &Path, _output: &Path) -> io::Result<()> {
            if self.replace_temp_before_link {
                fs::remove_file(temp)?;
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(temp)?
                    .write_all(b"replacement")?;
            }
            Ok(())
        }
    }

    fn reference() -> (aura_codec::SchemaDescriptor, ShadowEncodeResult) {
        let schema = SchemaBuilder::new("publication")
            .v3()
            .field("ts", FieldType::I64, FieldRole::Timestamp)
            .finish()
            .unwrap();
        let batch = AuraV3Batch {
            schema_id: schema.schema_id,
            row_count: 1,
            columns: vec![AuraV3Column {
                slot: 0,
                validity: None,
                values: AuraV3ColumnValues::I64(vec![42]),
            }],
        };
        let schema_fingerprint = canonical_v3_schema_fingerprint(&schema).unwrap();
        let logical_sha256 =
            canonical_v3_batch_sha256(&schema, &batch, V3ValueLimits::default()).unwrap();
        let block = encode_v3_value_block(&schema, &batch, V3ValueLimits::default()).unwrap();
        let block_sha256 = Sha256::digest(&block).into();
        (
            schema,
            ShadowEncodeResult {
                schema_fingerprint,
                row_count: 1,
                logical_sha256,
                block,
                block_sha256,
            },
        )
    }

    #[test]
    fn directory_open_failure_creates_nothing() {
        let dir = TestDirectory::new();
        let (schema, result) = reference();
        let ops = FaultOps {
            fail_open: true,
            ..Default::default()
        };
        assert!(publish_verified_block_with_ops(&dir.output(), &schema, &result, &ops).is_err());
        assert!(!dir.output().exists());
        assert!(dir.temp_paths().is_empty());
    }

    #[test]
    fn hardlink_collision_preserves_existing_and_removes_owned_temp() {
        let dir = TestDirectory::new();
        fs::write(dir.output(), b"winner").unwrap();
        let (schema, result) = reference();
        let ops = FaultOps::default();
        assert!(publish_verified_block_with_ops(&dir.output(), &schema, &result, &ops).is_err());
        assert_eq!(fs::read(dir.output()).unwrap(), b"winner");
        assert!(dir.temp_paths().is_empty());
    }

    #[test]
    fn hardlink_collision_with_unlink_failure_requires_coordinator_cleanup() {
        let dir = TestDirectory::new();
        fs::write(dir.output(), b"winner").unwrap();
        let (schema, result) = reference();
        let ops = FaultOps {
            fail_temp_remove_before_link: true,
            ..Default::default()
        };
        let error =
            publish_verified_block_with_ops(&dir.output(), &schema, &result, &ops).unwrap_err();
        assert_eq!(
            shadow_safe_error(&error.0).0,
            "publication_cleanup_required"
        );
        assert_eq!(fs::read(dir.output()).unwrap(), b"winner");
        let temps = dir.temp_paths();
        assert_eq!(temps.len(), 1);
        assert_eq!(fs::read(&temps[0]).unwrap(), result.block);
        assert_eq!(
            fs::metadata(&temps[0]).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn replaced_temp_identity_is_not_removed_or_published() {
        let dir = TestDirectory::new();
        let (schema, result) = reference();
        let ops = FaultOps {
            replace_temp_before_link: true,
            ..Default::default()
        };
        assert!(publish_verified_block_with_ops(&dir.output(), &schema, &result, &ops).is_err());
        assert!(!dir.output().exists());
        let temps = dir.temp_paths();
        assert_eq!(temps.len(), 1);
        assert_eq!(fs::read(&temps[0]).unwrap(), b"replacement");
    }

    #[test]
    fn commit_fsync_failure_rolls_back_to_no_final() {
        let dir = TestDirectory::new();
        let (schema, result) = reference();
        let ops = FaultOps {
            fail_sync_call: Some(1),
            ..Default::default()
        };
        let error =
            publish_verified_block_with_ops(&dir.output(), &schema, &result, &ops).unwrap_err();
        assert_ne!(shadow_safe_error(&error.0).0, "publication_ambiguous");
        assert!(!dir.output().exists());
        assert!(dir.temp_paths().is_empty());
    }

    #[test]
    fn rollback_failure_reports_publication_ambiguous_with_final_present() {
        let dir = TestDirectory::new();
        let (schema, result) = reference();
        let ops = FaultOps {
            fail_sync_call: Some(1),
            fail_all_remove_after_link: true,
            ..Default::default()
        };
        let error =
            publish_verified_block_with_ops(&dir.output(), &schema, &result, &ops).unwrap_err();
        assert_eq!(shadow_safe_error(&error.0).0, "publication_ambiguous");
        assert!(dir.output().exists());
        assert_eq!(fs::read(dir.output()).unwrap(), result.block);
    }

    #[test]
    fn committed_final_survives_temp_unlink_failure_with_stale_flag() {
        let dir = TestDirectory::new();
        let (schema, result) = reference();
        let ops = FaultOps {
            fail_temp_remove_after_link: true,
            ..Default::default()
        };
        let outcome =
            publish_verified_block_with_ops(&dir.output(), &schema, &result, &ops).unwrap();
        assert!(outcome.stale_temp_cleanup_required);
        assert_eq!(fs::read(dir.output()).unwrap(), result.block);
        assert_eq!(dir.temp_paths().len(), 1);
    }

    #[test]
    fn cleanup_fsync_failure_reports_success_with_stale_flag() {
        let dir = TestDirectory::new();
        let (schema, result) = reference();
        let ops = FaultOps {
            fail_sync_call: Some(2),
            ..Default::default()
        };
        let outcome =
            publish_verified_block_with_ops(&dir.output(), &schema, &result, &ops).unwrap();
        assert!(outcome.stale_temp_cleanup_required);
        assert_eq!(fs::read(dir.output()).unwrap(), result.block);
        assert!(dir.temp_paths().is_empty());
    }

    #[test]
    fn group_or_world_writable_parent_is_rejected_before_temp_creation() {
        let dir = TestDirectory::new();
        fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o770)).unwrap();
        let (schema, result) = reference();
        assert!(publish_verified_block_with_ops(
            &dir.output(),
            &schema,
            &result,
            &FaultOps::default()
        )
        .is_err());
        assert!(!dir.output().exists());
        assert!(dir.temp_paths().is_empty());
    }
}
