//! Retries a plain HTTP GET against a real socket, using a `retryctl` policy
//! to decide how long to wait between attempts. No TLS, no redirects, no
//! chunked transfer decoding: just enough of HTTP/1.1 to prove the policy
//! wiring works end to end against something that can actually fail.
//!
//! Usage:
//!   cargo run --example http_retry -- <host> <port> <path> [policy_file]
//!
//! `policy_file` is optional; without it a small built-in policy is used.
//! Point this at a server that sometimes answers with a 5xx or drops the
//! connection to see the retry loop and its waits in action.

use std::env;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use retryctl::{parse_policy, RetryPolicy};

const DEFAULT_POLICY: &str = "\
max_attempts=5
base_delay_ms=250
max_delay_ms=8000
strategy=exponential
multiplier=2.0
jitter=full
";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: http_retry <host> <port> <path> [policy_file]");
        return ExitCode::FAILURE;
    }
    let host = &args[1];
    let path = &args[3];

    let port: u16 = match args[2].parse() {
        Ok(p) => p,
        Err(_) => {
            eprintln!("error: invalid port {:?}", args[2]);
            return ExitCode::FAILURE;
        }
    };

    let policy = match load_policy(args.get(4).map(String::as_str)) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    match run(host, port, path, &policy) {
        Ok(status) => {
            println!("final status: {status}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn load_policy(path: Option<&str>) -> Result<RetryPolicy, String> {
    let text = match path {
        Some(path) => {
            std::fs::read_to_string(path).map_err(|e| format!("reading {path}: {e}"))?
        }
        None => DEFAULT_POLICY.to_string(),
    };
    parse_policy(&text)
}

/// Runs the retry loop against `host:port`, returning the response status
/// line on the first non-retryable answer, or an error once the policy's
/// attempts or deadline run out.
fn run(host: &str, port: u16, path: &str, policy: &RetryPolicy) -> Result<String, String> {
    let mut rng_state = seed_from_clock();
    let mut prev_delay = policy.base_delay;
    let mut elapsed = Duration::ZERO;

    for attempt in 1..=policy.max_attempts {
        match fetch(host, port, path) {
            Ok(status) if is_retryable_status(&status) => {
                eprintln!("attempt {attempt}: http {status}");
            }
            Ok(status) => return Ok(status),
            Err(e) => eprintln!("attempt {attempt}: {e}"),
        }

        if attempt == policy.max_attempts {
            break;
        }
        let delay = policy.delay_for_attempt(attempt, &mut rng_state, &mut prev_delay);
        if policy.deadline_exceeded(elapsed + delay) {
            return Err("retry budget exhausted".to_string());
        }
        elapsed += delay;
        eprintln!("waiting {delay:?} before attempt {}", attempt + 1);
        thread::sleep(delay);
    }

    Err(format!("gave up after {} attempt(s)", policy.max_attempts))
}

/// Opens a fresh connection, sends a minimal `Connection: close` GET, and
/// reads the response to EOF. A new connection per attempt keeps this
/// example honest about what a real retry does: it doesn't assume the old
/// socket is still worth anything after a failure.
fn fetch(host: &str, port: u16, path: &str) -> Result<String, String> {
    let mut stream =
        TcpStream::connect((host, port)).map_err(|e| format!("connect failed: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| format!("setting read timeout: {e}"))?;

    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nUser-Agent: retryctl-example\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("write failed: {e}"))?;

    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .map_err(|e| format!("read failed: {e}"))?;

    let text = String::from_utf8_lossy(&response);
    let status_line = text.lines().next().ok_or("empty response")?;
    status_line
        .split_whitespace()
        .nth(1)
        .map(str::to_string)
        .ok_or_else(|| format!("malformed status line {status_line:?}"))
}

/// 5xx and 429 are worth retrying; anything else that came back with a
/// status line is a final answer as far as this example cares.
fn is_retryable_status(status: &str) -> bool {
    status.starts_with('5') || status == "429"
}

/// Seeds the jitter RNG from the clock so repeated runs of this example
/// don't all draw the same "random" delays. Not suitable for anything
/// where predictability would matter.
fn seed_from_clock() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    if nanos == 0 {
        0x9E3779B97F4A7C15
    } else {
        nanos
    }
}
