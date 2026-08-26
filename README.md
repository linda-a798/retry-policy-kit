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
```

`max_delay_ms` caps the computed wait before jitter is applied. `jitter=full`
picks a uniform random delay between 0 and that capped value on each call,
which avoids synchronized retry storms across many clients. `jitter=decorrelated`
instead draws each delay from `[base_delay_ms, previous_delay * 3]`, capped at
`max_delay_ms`; it ignores `strategy` and `multiplier` entirely since the
recurrence already determines the growth. See `examples/policy.conf` for a
complete example.

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

## Status

Early skeleton. The library currently supports fixed and exponential
backoff, with none, full, or decorrelated jitter. See the issue tracker for
what's missing before this is worth depending on.

## License

MIT, see `LICENSE`.
