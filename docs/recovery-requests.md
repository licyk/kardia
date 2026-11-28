# Recovery requests from hanafubuki-agent

hanafubuki-agent (the downstream host) runs a supervised set of plugin rows:
each row is a plugin mounted by the host, and a row that faults is restarted,
or left failed, by the host's own policy. This document lists what that host
needs from `kardia-core`'s recovery so it can drop its own copy of the
supervision and use the lifecycle's, as `AGENTS.md` asks of every plugin.

Today the host implements, in `crates/host/src/host/restarts.rs` and
`crates/host/src/restart.rs`:

* a **delay table** — the n-th automatic restart within a window waits
  `delays[n]` (2 s, 10 s, 30 s);
* a **restart window** — three restarts in 10 minutes; once they are used up
  the row stays failed until the user retries it, which also empties the
  window;
* a **next-restart time** the interface shows (`restarts_in_window`,
  `next_restart_at`), so a row that keeps failing is visible before it stops;
* **cancellation** — turning the row off, saving a new configuration, a
  manual retry and host shutdown each cancel a pending restart;
* **a start that fails is not restarted by itself**: a constructor that fails
  will fail again, so it stays failed until a retry. Only a fault of a
  *running* instance is retried.

`RecoveryPolicy` in `kardia-core` covers part of this: `Retry { delay,
max_retries, backoff }` retries a failed fiber, `Backoff::Exponential` grows
the delay, and `RecoveryPolicy::Host(handler)` lets a host decide. The four
gaps below are what keeps the host on its own implementation.

## R1. The policy must see why the fiber failed

`RecoveryPolicy::Host(handler)` receives a `Failure { fiber, error }`. That is
enough to *decide*, but the decision the host needs is not the same for every
failure:

* a fault of a running instance — retry it;
* a start that failed (the constructor's `apply` returned an error or
  panicked) — do **not** retry it on its own;
* `ValidationError` — never retry (kardia already does this).

The host cannot tell the first two apart from `Error` alone: both arrive as a
failed fiber with an error. Request: a `Failure` (or the policy's input) that
says **what failed** — at least `phase: Start | Running | Stop | Disposing`
beside the fiber and the error. `kardia-core` knows which phase it was in; the
host does not.

The supervisor restarts through `fiber.update_with(fiber.config(), true)`, so a
restart re-applies the plugin rather than re-entering the same instance: a
constructor that fails will fail the same way on every retry, which is exactly
what the host refuses to do. The phase is what lets the host say so through
the policy instead of duplicating the check.

## R2. A window, not only a count

`max_retries` counts every retry for the life of the fiber. The host's policy
is a **window**: after the delays are exhausted the row stays failed, but a
manual retry, or a long enough quiet period, starts the count again. A fiber
that faults once a day should not stay failed forever on its fourth fault.

Request: either

* a policy that takes a list of delays (`Delays(&[Duration])`) rather than one
  delay and a count, so the n-th retry waits the n-th entry and the list's end
  is the limit; or
* `RecoveryPolicy::Host` given enough to implement it — the failure's phase
  (R1) and the attempt number — plus a documented way for the host to reset a
  fiber's retry state after a manual retry.

## R3. The host must see and cancel a pending retry

`RecoveryAction::RetryAfter(delay)` schedules a restart, and the fiber then
restarts on its own. The host needs to:

* **show** when the next restart is due, and how many have happened;
* **cancel** it when the row is turned off, its configuration changes, or the
  host shuts down, so a pending restart never races the row being replaced;
* **retry at once** when the user asks, without waiting for the pending delay.

Today `Fiber::set_recovery` cancels a previous pending action when it is
called again, but the host has no way to read whether one is pending, when it
will run, or how many have run. Request: a read on the fiber (or an event)
that reports the pending retry and the attempts so far, and a
`Fiber::retry_now()` (or `set_recovery` returning to `Manual` and disposing
the old instance, documented) that performs the pending restart immediately.
An event on the lifecycle would be ideal — the host already listens to
`StatusChanged` for the rest of the row's state.

## R4. Cancellation and disposal must not be retried

`AGENTS.md` already requires that explicit disposal, disabling or removal
cancel pending recovery. The host cancels by dropping its own scheduled task;
if the retry is kardia's, the host needs to be sure that disposing a fiber, or
withdrawing its plugin, cancels a pending retry rather than restarting the
fiber once more. This is a correctness requirement, not a new feature: a
restart after disposal would remount a row the user turned off.

## What the host would look like afterwards

With R1 to R4, the host deletes `RestartWindow`, `RestartPolicy` and the
scheduled-restart task, and sets on each row's fiber:

```rust
fiber.set_recovery(RecoveryPolicy::retry_with(
    Duration::from_secs(2),
    Some(3),
    Backoff::Exponential, // 2 s, 4 s, 8 s — the list the host hard-codes today
)));
```

plus a host handler for the start-failure rule of R1, and reads the pending
retry from R3 for the interface. Everything else — isolation, cleanup before
the replacement, unrelated plugins continuing — is kardia's already and would
then be the only implementation in play.

## Not requested

* Retrying the root fiber: the host never does (a fault of the root is the
  host's own).
* Retrying `ValidationError`: kardia already refuses, and the host agrees.
* A configuration format: the host writes its own `[host]` section; only the
  policy API matters to it.
