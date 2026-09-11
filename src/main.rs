use std::env;
use std::fs;
use std::io::{self, Read};
use std::process::ExitCode;
use std::time::Duration;

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
    let mut elapsed = Duration::ZERO;
    let mut schedule: Vec<(u32, Duration)> = Vec::new();
    let mut budget_exhausted = false;
    for attempt in 1..policy.max_attempts {
        let delay = policy.delay_for_attempt(attempt, &mut rng_state, &mut prev_delay);
        elapsed += delay;
        if policy.deadline_exceeded(elapsed) {
            budget_exhausted = true;
            break;
        }
        schedule.push((attempt + 1, delay));
    }

    match format {
        Format::Text => {
            println!("max_attempts: {}", policy.max_attempts);
            for (before_attempt, delay) in &schedule {
                println!("  before attempt {before_attempt}: wait {delay:?}");
            }
            if budget_exhausted {
                println!("  retry budget exhausted before schedule reached max_attempts");
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
            let deadline_ms = policy
                .deadline
                .map(|d| d.as_millis().to_string())
                .unwrap_or_else(|| "null".to_string());
            println!(
                r#"{{"max_attempts":{},"deadline_ms":{deadline_ms},"budget_exhausted":{budget_exhausted},"schedule":[{}]}}"#,
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
    Failed { wait: Duration },
    Exhausted { reason: &'static str },
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
    let mut elapsed = Duration::ZERO;
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
                    history.push((attempt, AttemptOutcome::Exhausted { reason: "no attempts left" }));
                    ran_out = false;
                    break;
                }
                let delay = policy.delay_for_attempt(attempt, &mut rng_state, &mut prev_delay);
                if policy.deadline_exceeded(elapsed + delay) {
                    history.push((
                        attempt,
                        AttemptOutcome::Exhausted { reason: "retry budget exhausted" },
                    ));
                    ran_out = false;
                    break;
                }
                elapsed += delay;
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
                    AttemptOutcome::Exhausted { reason } => {
                        println!("attempt {attempt}: failed, {reason}, giving up")
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
                    AttemptOutcome::Exhausted { reason } => {
                        format!(r#"{{"attempt":{attempt},"outcome":"exhausted","reason":"{reason}"}}"#)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_flag_finds_value_after_name() {
        let a = args(&["plan", "--policy", "examples/policy.conf"]);
        assert_eq!(parse_flag(&a, "--policy"), Some("examples/policy.conf".to_string()));
    }

    #[test]
    fn parse_flag_missing_returns_none() {
        let a = args(&["plan"]);
        assert_eq!(parse_flag(&a, "--policy"), None);
    }

    #[test]
    fn parse_flag_at_end_with_no_value_returns_none() {
        let a = args(&["plan", "--policy"]);
        assert_eq!(parse_flag(&a, "--policy"), None);
    }

    #[test]
    fn parse_flag_uses_first_occurrence() {
        let a = args(&["plan", "--policy", "a.conf", "--policy", "b.conf"]);
        assert_eq!(parse_flag(&a, "--policy"), Some("a.conf".to_string()));
    }

    #[test]
    fn parse_format_defaults_to_text() {
        let a = args(&["plan"]);
        assert_eq!(parse_format(&a).unwrap(), Format::Text);
    }

    #[test]
    fn parse_format_reads_json() {
        let a = args(&["plan", "--format", "json"]);
        assert_eq!(parse_format(&a).unwrap(), Format::Json);
    }

    #[test]
    fn parse_format_rejects_unknown_value() {
        let a = args(&["plan", "--format", "yaml"]);
        let err = parse_format(&a).unwrap_err();
        assert!(err.contains("unknown --format"));
    }
}
