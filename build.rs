use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const GIT_OUTPUT_LIMIT: usize = 64 * 1024;
const LOCKFILE_LIMIT: u64 = 1024 * 1024;
const GIT_TIMEOUT: Duration = Duration::from_secs(2);

fn main() {
    println!("cargo:rerun-if-env-changed=AURA_BUILD_GIT_COMMIT_OVERRIDE");
    println!("cargo:rerun-if-env-changed=AURA_BUILD_GIT_DIRTY_OVERRIDE");
    println!("cargo:rerun-if-changed=Cargo.lock");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=tests");
    println!("cargo:rerun-if-changed=examples");
    println!("cargo:rerun-if-changed=docs");
    println!("cargo:rerun-if-changed=README.md");

    let manifest = env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default();
    let manifest = Path::new(&manifest);
    emit_head_ref_rerun(manifest);

    let override_commit = env::var("AURA_BUILD_GIT_COMMIT_OVERRIDE").ok();
    let override_dirty = env::var("AURA_BUILD_GIT_DIRTY_OVERRIDE").ok();
    let (provenance, source) = match (override_commit, override_dirty) {
        (None, None) => (local_git_provenance(manifest), "git-informational"),
        (Some(commit), Some(dirty)) if valid_commit(&commit) => {
            let dirty = parse_bool(&dirty)
                .unwrap_or_else(|| panic!("AURA_BUILD_GIT_DIRTY_OVERRIDE must be true or false"));
            (Some((commit, dirty)), "override-untrusted")
        }
        (Some(_), Some(_)) => {
            panic!("AURA_BUILD_GIT_COMMIT_OVERRIDE must be 40 lowercase hexadecimal characters")
        }
        _ => panic!("both Aura build provenance override variables must be supplied together"),
    };
    match provenance {
        Some((commit, dirty)) => {
            println!("cargo:rustc-env=AURA_EMBEDDED_GIT_COMMIT={commit}");
            println!("cargo:rustc-env=AURA_EMBEDDED_GIT_DIRTY={dirty}");
            println!("cargo:rustc-env=AURA_EMBEDDED_PROVENANCE_SOURCE={source}");
        }
        None => {
            println!("cargo:rustc-env=AURA_EMBEDDED_GIT_COMMIT=unavailable");
            println!("cargo:rustc-env=AURA_EMBEDDED_GIT_DIRTY=unavailable");
            println!("cargo:rustc-env=AURA_EMBEDDED_PROVENANCE_SOURCE=unavailable");
        }
    }

    let arrow_version =
        locked_package_version(manifest, "arrow").unwrap_or_else(|| "unavailable".to_owned());
    println!("cargo:rustc-env=AURA_EMBEDDED_ARROW_VERSION={arrow_version}");
}

fn valid_commit(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn emit_head_ref_rerun(manifest: &Path) {
    let Some(git_dir) = resolve_git_dir(manifest) else {
        return;
    };
    println!("cargo:rerun-if-changed={}", git_dir.join("HEAD").display());
    println!("cargo:rerun-if-changed={}", git_dir.join("index").display());
    let Ok(head) = fs::read_to_string(git_dir.join("HEAD")) else {
        return;
    };
    let Some(reference) = head.trim().strip_prefix("ref: ") else {
        return;
    };
    if !reference.is_empty()
        && reference
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/-_.".contains(&byte))
    {
        println!(
            "cargo:rerun-if-changed={}",
            git_dir.join(reference).display()
        );
    }
}

fn resolve_git_dir(manifest: &Path) -> Option<PathBuf> {
    let dot_git = manifest.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    let link = fs::read_to_string(dot_git).ok()?;
    let path = link.trim().strip_prefix("gitdir: ")?;
    let path = Path::new(path);
    Some(if path.is_absolute() {
        path.to_owned()
    } else {
        manifest.join(path)
    })
}

fn local_git_provenance(manifest: &Path) -> Option<(String, bool)> {
    let commit = bounded_git(manifest, &["rev-parse", "--verify", "HEAD"])?;
    let commit = commit.trim();
    if !valid_commit(commit) {
        return None;
    }
    let status = bounded_git(
        manifest,
        &[
            "status",
            "--porcelain=v1",
            "--untracked-files=normal",
            "--ignore-submodules=all",
        ],
    )?;
    Some((commit.to_owned(), !status.is_empty()))
}

fn bounded_git(manifest: &Path, args: &[&str]) -> Option<String> {
    let git = ["/usr/bin/git", "/bin/git"]
        .into_iter()
        .find(|path| Path::new(path).is_file())?;
    let mut child = Command::new(git)
        .env_clear()
        .env("LANG", "C")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .arg("-c")
        .arg("core.fsmonitor=false")
        .arg("-c")
        .arg("core.untrackedCache=false")
        .arg("-C")
        .arg(manifest)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout
            .take((GIT_OUTPUT_LIMIT + 1) as u64)
            .read_to_end(&mut bytes);
        (result, bytes)
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().ok()? {
            break status;
        }
        if started.elapsed() >= GIT_TIMEOUT {
            let _ = child.kill();
            let _ = child.try_wait();
            return None;
        }
        thread::sleep(Duration::from_millis(10));
    };
    let (read_result, bytes) = reader.join().ok()?;
    if read_result.is_err() || bytes.len() > GIT_OUTPUT_LIMIT || !status.success() {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn locked_package_version(manifest: &Path, package: &str) -> Option<String> {
    let file = fs::File::open(manifest.join("Cargo.lock")).ok()?;
    if file.metadata().ok()?.len() > LOCKFILE_LIMIT {
        return None;
    }
    let mut text = String::new();
    file.take(LOCKFILE_LIMIT + 1)
        .read_to_string(&mut text)
        .ok()?;
    let mut in_package = false;
    let mut name_matches = false;
    for line in text.lines() {
        if line == "[[package]]" {
            in_package = true;
            name_matches = false;
        } else if in_package && line == format!("name = \"{package}\"") {
            name_matches = true;
        } else if in_package && name_matches {
            if let Some(version) = line
                .strip_prefix("version = \"")
                .and_then(|value| value.strip_suffix('"'))
            {
                return Some(version.to_owned());
            }
        }
    }
    None
}
