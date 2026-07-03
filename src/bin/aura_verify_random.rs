use std::env;
use std::fs;
use std::path::PathBuf;

use aura_codec::random_verify::{run_random_verification, RandomVerifyConfig};

fn main() {
    let mut config = RandomVerifyConfig::default();
    let mut output = None::<PathBuf>;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--cases" => config.cases = parse_next(&mut args, "--cases"),
            "--max-records" => config.max_records = parse_next(&mut args, "--max-records"),
            "--seed" => config.seed = parse_next(&mut args, "--seed"),
            "--output" => output = Some(PathBuf::from(next_value(&mut args, "--output"))),
            "--check-cross-format" => config.check_cross_format = true,
            "--no-cross-format" => config.check_cross_format = false,
            "--check-streaming" => config.check_streaming = true,
            "--no-streaming" => config.check_streaming = false,
            "--check-replay" => config.check_replay = true,
            "--no-replay" => config.check_replay = false,
            "--check-orderbook" => config.check_orderbook = true,
            "--no-orderbook" => config.check_orderbook = false,
            "--formats" => {
                let _ = next_value(&mut args, "--formats");
            }
            "--help" | "-h" => {
                print_help();
                return;
            }
            other => {
                eprintln!("unknown argument: {other}");
                print_help();
                std::process::exit(2);
            }
        }
    }

    let report = run_random_verification(config);
    let json = serde_json::to_string_pretty(&report.to_json()).expect("report json");
    if let Some(path) = output {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create report directory");
        }
        fs::write(&path, json.as_bytes()).expect("write report");
        println!("{}", path.display());
    } else {
        println!("{json}");
    }

    if !report.failures.is_empty() {
        std::process::exit(1);
    }
}

fn print_help() {
    println!(
        "aura-verify-random --cases 100 --max-records 10000 --seed 12345 --output /tmp/aura-random-verify/report.json"
    );
}

fn parse_next<T>(args: &mut impl Iterator<Item = String>, name: &'static str) -> T
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    let value = next_value(args, name);
    value
        .parse::<T>()
        .unwrap_or_else(|error| panic!("{name} parse error for {value}: {error}"))
}

fn next_value(args: &mut impl Iterator<Item = String>, name: &'static str) -> String {
    args.next()
        .unwrap_or_else(|| panic!("{name} requires a value"))
}
