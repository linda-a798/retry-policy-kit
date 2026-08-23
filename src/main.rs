use std::env;
use std::fs;
use std::io::{self, Read};
use std::process::ExitCode;

use retryctl::parse_policy;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("error: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    match args.get(1).map(String::as_str).unwrap_or("") {
        "plan" => cmd_plan(&args[2..]),
        "simulate" => cmd_simulate(&args[2..]),
        "" | "help" | "--help" | "-h" => {
            print_usage();
            Ok(())
        }
        other => Err(format!("unknown command {other:?}, try 'help'")),
    }
}

fn print_usage() {
    print!(concat!(
        "retryctl - inspect and simulate retry policies\n\n",
        "USAGE:\n",
        "  retryctl plan [--policy <path|->]\n",
        "  retryctl simulate --events <path|-> [--policy <path|->]\n\n",
        "A policy is a text file of key=value lines; see README.md for the\n",
        "format. Pass '-' (or omit --policy) to read the policy from stdin.\n",
        "Exactly one of --policy / --events may come from stdin at a time.\n",
    ));
}

/// Reads all of stdin when `source` is "-", otherwise reads the named file.
/// This is the one piece of plumbing every subcommand routes through, so
/// both the policy and the event list can come from either place.
fn read_input(source: &str) -> io::Result<String> {
    if source == "-" {
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf)?;
        Ok(buf)
    } else {
        fs::read_to_string(source)
    }
}

fn parse_flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn cmd_plan(args: &[String]) -> Result<(), String> {
    let policy_source = parse_flag(args, "--policy").unwrap_or_else(|| "-".to_string());
    let text = read_input(&policy_source)
        .map_err(|e| format!("reading policy from {policy_source}: {e}"))?;
    let policy = parse_policy(&text)?;

    // Seed is fixed so `plan` output is reproducible run to run; it's a
    // preview of the schedule shape, not a claim about real jitter draws.
    let mut rng_state = 0x9E3779B97F4A7C15u64;
    println!("max_attempts: {}", policy.max_attempts);
    for attempt in 1..policy.max_attempts {
        let delay = policy.delay_for_attempt(attempt, &mut rng_state);
        println!("  before attempt {}: wait {:?}", attempt + 1, delay);
    }
    Ok(())
}

fn cmd_simulate(args: &[String]) -> Result<(), String> {
    let policy_source = parse_flag(args, "--policy").unwrap_or_else(|| "-".to_string());
    let events_source =
        parse_flag(args, "--events").ok_or("simulate requires --events <path|->")?;

    if policy_source == "-" && events_source == "-" {
        return Err("policy and events cannot both read from stdin".to_string());
    }

    let policy_text = read_input(&policy_source)
        .map_err(|e| format!("reading policy from {policy_source}: {e}"))?;
    let policy = parse_policy(&policy_text)?;

    let events_text = read_input(&events_source)
        .map_err(|e| format!("reading events from {events_source}: {e}"))?;

    let mut rng_state = 0x9E3779B97F4A7C15u64;
    let mut attempt = 0u32;
    for raw_line in events_text.lines() {
        let outcome = raw_line.trim();
        if outcome.is_empty() || outcome.starts_with('#') {
            continue;
        }
        attempt += 1;
        match outcome {
            "ok" | "success" => {
                println!("attempt {attempt}: succeeded");
                return Ok(());
            }
            "fail" | "failure" => {
                if attempt >= policy.max_attempts {
                    println!("attempt {attempt}: failed, no attempts left, giving up");
                    return Ok(());
                }
                let delay = policy.delay_for_attempt(attempt, &mut rng_state);
                println!("attempt {attempt}: failed, waiting {delay:?} before retry");
            }
            other => return Err(format!("unrecognized event {other:?}, expected 'ok' or 'fail'")),
        }
    }
    println!("ran out of events after {attempt} attempt(s) without success or exhaustion");
    Ok(())
}
