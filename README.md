# retryctl

Most retry logic gets written inline, once, for whatever call is failing that
week, and then never looked at again. It's usually a `for` loop with a
hardcoded sleep, no cap on the delay, no jitter, and no way to check what the
actual wait schedule looks like before it's running against production. This
is a small library for describing a retry policy as data, plus a CLI for
previewing and replaying that policy without having to run the real system
it's protecting.

`retryctl` doesn't perform retries for you (it doesn't make HTTP calls or
know about your I/O) — it computes delay schedules and can replay a recorded
sequence of successes and failures against a policy. The library
(`retryctl::parse_policy`, `retryctl::RetryPolicy`) is meant to be pulled into
a real client's retry loop; the CLI is for checking a policy's behavior by
hand.

## Building

```
cargo build --release
```

No third-party dependencies, so there's nothing to fetch first.

## Policy format

A policy is a text file of `key=value` lines:

```
max_attempts=5
base_delay_ms=250
max_delay_ms=8000
strategy=exponential   # or "fixed"
multiplier=2.0         # only read when strategy=exponential
jitter=full            # or "none", "decorrelated"
deadline_ms=5000       # optional; total wait budget across all attempts
```

`max_delay_ms` caps the computed wait before jitter is applied. `jitter=full`
picks a uniform random delay between 0 and that capped value on each call,
which avoids synchronized retry storms across many clients. `jitter=decorrelated`
instead draws each delay from `[base_delay_ms, previous_delay * 3]`, capped at
`max_delay_ms`; it ignores `strategy` and `multiplier` entirely since the
recurrence already determines the growth. See `examples/policy.conf` for a
complete example.

`deadline_ms` is a retry budget separate from `max_attempts`: it caps the
total time spent waiting between attempts, not the wall-clock time since the
first call. A policy gives up as soon as either bound is hit, whichever
comes first. This matters when the caller has its own timeout (an HTTP
client deadline, a job's remaining time slice) and retrying past that point
just wastes the attempts it has left. Leaving it unset means only
`max_attempts` bounds the policy, which is the default.

## Usage

Preview the wait schedule for a policy file:

```
$ retryctl plan --policy examples/policy.conf
max_attempts: 5
  before attempt 2: wait 250ms
  before attempt 3: wait 500ms
  before attempt 4: wait 1s
  before attempt 5: wait 2s
```

Policies can also come from stdin, which is handy for piping in output from
something else or editing on the fly:

```
$ cat examples/policy.conf | retryctl plan
```

Replay a recorded sequence of call outcomes against a policy to see when it
would give up:

```
$ retryctl simulate --policy examples/policy.conf --events examples/events.txt
attempt 1: failed, waiting 187ms before retry
attempt 2: failed, waiting 412ms before retry
attempt 3: succeeded
```

Exactly one of `--policy` or `--events` may be `-` (stdin) in a single
invocation, since stdin can only be consumed once:

```
$ cat examples/events.txt | retryctl simulate --policy examples/policy.conf --events -
```

Both subcommands take `--format json` for scripting, instead of the default
human-readable text:

```
$ retryctl plan --policy examples/policy.conf --format json
{"max_attempts":5,"deadline_ms":null,"budget_exhausted":false,"schedule":[{"before_attempt":2,"wait_ms":250},{"before_attempt":3,"wait_ms":500},{"before_attempt":4,"wait_ms":1000},{"before_attempt":5,"wait_ms":2000}]}

$ retryctl simulate --policy examples/policy.conf --events examples/events.txt --format json
{"result":"complete","attempts":[{"attempt":1,"outcome":"failed","wait_ms":187},{"attempt":2,"outcome":"failed","wait_ms":412},{"attempt":3,"outcome":"succeeded"}]}
```

`result` is `"complete"` when the events ran to a success or an exhausted
policy, or `"incomplete"` if the event file ran out first.

## Example: retrying a real HTTP call

`examples/http_retry.rs` wires a policy into an actual retry loop against a
real socket, using only `std::net::TcpStream` — no HTTP client dependency,
just enough of HTTP/1.1 to send a GET and read a status line back:

```
$ cargo run --example http_retry -- example.com 80 /
```

A 4th argument points it at a policy file; without one it falls back to a
small built-in policy. 5xx responses and 429 are treated as retryable; any
other status is returned immediately. This is meant as a worked example of
wiring `retryctl::RetryPolicy` into a real caller, not as a general-purpose
HTTP client.

## Status

Early skeleton. The library currently supports fixed and exponential
backoff, with none, full, or decorrelated jitter. See the issue tracker for
what's missing before this is worth depending on.

## License

MIT, see `LICENSE`.
