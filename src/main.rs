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
        "  retryctl plan [--policy <path|->] [--format text|json]\n",
        "  retryctl simulate --events <path|-> [--policy <path|->] [--format text|json]\n\n",
        "A policy is a text file of key=value lines; see README.md for the\n",
        "format. Pass '-' (or omit --policy) to read the policy from stdin.\n",
        "Exactly one of --policy / --events may come from stdin at a time.\n",
        "--format defaults to text; json is meant for piping into other tools.\n",
    ));
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Format {
    Text,
    Json,
}

fn parse_format(args: &[String]) -> Result<Format, String> {
    match parse_flag(args, "--format").as_deref() {
        None | Some("text") => Ok(Format::Text),
        Some("json") => Ok(Format::Json),
        Some(other) => Err(format!("unknown --format {other:?}, expected text or json")),
    }
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
    let format = parse_format(args)?;
    let text = read_input(&policy_source)
        .map_err(|e| format!("reading policy from {policy_source}: {e}"))?;
    let policy = parse_policy(&text)?;

    // Seed is fixed so `plan` output is reproducible run to run; it's a
    // preview of the schedule shape, not a claim about real jitter draws.
    let mut rng_state = 0x9E3779B97F4A7C15u64;
    let mut prev_delay = policy.base_delay;
    let schedule: Vec<(u32, std::time::Duration)> = (1..policy.max_attempts)
        .map(|attempt| {
            let delay = policy.delay_for_attempt(attempt, &mut rng_state, &mut prev_delay);
            (attempt + 1, delay)
        })
        .collect();

    match format {
        Format::Text => {
            println!("max_attempts: {}", policy.max_attempts);
            for (before_attempt, delay) in &schedule {
                println!("  before attempt {before_attempt}: wait {delay:?}");
            }
        }
        Format::Json => {
            let entries: Vec<String> = schedule
                .iter()
                .map(|(before_attempt, delay)| {
                    format!(
                        r#"{{"before_attempt":{before_attempt},"wait_ms":{}}}"#,
                        delay.as_millis()
                    )
                })
                .collect();
            println!(
                r#"{{"max_attempts":{},"schedule":[{}]}}"#,
                policy.max_attempts,
                entries.join(",")
            );
        }
    }
    Ok(())
}

/// One line of simulated history: what happened on a given attempt, and how
/// long the policy said to wait afterward (if it retried at all).
enum AttemptOutcome {
    Succeeded,
    Failed { wait: std::time::Duration },
    Exhausted,
}

fn cmd_simulate(args: &[String]) -> Result<(), String> {
    let policy_source = parse_flag(args, "--policy").unwrap_or_else(|| "-".to_string());
    let events_source =
        parse_flag(args, "--events").ok_or("simulate requires --events <path|->")?;
    let format = parse_format(args)?;

    if policy_source == "-" && events_source == "-" {
        return Err("policy and events cannot both read from stdin".to_string());
    }

    let policy_text = read_input(&policy_source)
        .map_err(|e| format!("reading policy from {policy_source}: {e}"))?;
    let policy = parse_policy(&policy_text)?;

    let events_text = read_input(&events_source)
        .map_err(|e| format!("reading events from {events_source}: {e}"))?;

    let mut rng_state = 0x9E3779B97F4A7C15u64;
    let mut prev_delay = policy.base_delay;
    let mut attempt = 0u32;
    let mut history: Vec<(u32, AttemptOutcome)> = Vec::new();
    let mut ran_out = true;
    for raw_line in events_text.lines() {
        let outcome = raw_line.trim();
        if outcome.is_empty() || outcome.starts_with('#') {
            continue;
        }
        attempt += 1;
        match outcome {
            "ok" | "success" => {
                history.push((attempt, AttemptOutcome::Succeeded));
                ran_out = false;
                break;
            }
            "fail" | "failure" => {
                if attempt >= policy.max_attempts {
                    history.push((attempt, AttemptOutcome::Exhausted));
                    ran_out = false;
                    break;
                }
                let delay = policy.delay_for_attempt(attempt, &mut rng_state, &mut prev_delay);
                history.push((attempt, AttemptOutcome::Failed { wait: delay }));
            }
            other => return Err(format!("unrecognized event {other:?}, expected 'ok' or 'fail'")),
        }
    }

    match format {
        Format::Text => {
            for (attempt, outcome) in &history {
                match outcome {
                    AttemptOutcome::Succeeded => println!("attempt {attempt}: succeeded"),
                    AttemptOutcome::Failed { wait } => {
                        println!("attempt {attempt}: failed, waiting {wait:?} before retry")
                    }
                    AttemptOutcome::Exhausted => {
                        println!("attempt {attempt}: failed, no attempts left, giving up")
                    }
                }
            }
            if ran_out {
                println!("ran out of events after {attempt} attempt(s) without success or exhaustion");
            }
        }
        Format::Json => {
            let entries: Vec<String> = history
                .iter()
                .map(|(attempt, outcome)| match outcome {
                    AttemptOutcome::Succeeded => {
                        format!(r#"{{"attempt":{attempt},"outcome":"succeeded"}}"#)
                    }
                    AttemptOutcome::Failed { wait } => format!(
                        r#"{{"attempt":{attempt},"outcome":"failed","wait_ms":{}}}"#,
                        wait.as_millis()
                    ),
                    AttemptOutcome::Exhausted => {
                        format!(r#"{{"attempt":{attempt},"outcome":"exhausted"}}"#)
                    }
                })
                .collect();
            let result = if ran_out { "incomplete" } else { "complete" };
            println!(
                r#"{{"result":"{result}","attempts":[{}]}}"#,
                entries.join(",")
            );
        }
    }
    Ok(())
}
