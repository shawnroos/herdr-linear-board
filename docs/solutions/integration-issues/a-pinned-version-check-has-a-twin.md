# A pinned version check has a twin

**The board compares the Herdr version in two places, and fixing one leaves the
other refusing the build it was told to accept.**

`crates/board-herdr/src/client.rs` gates the runtime connection on the pinned
Herdr release, and `e2e/lib.sh` gates every e2e scenario on the same release —
twice, once on `herdr --version` and once on what the socket's `ping` reports.
The four opt-in real-provider smokes (`real-pi`, `real-claude-haiku`,
`real-codex`, `real-opencode`) each compare twice more, the same two ways.
Eleven comparisons, one rule.

Commit `6acb8b1` fixed the runtime one: a preview build (`0.9.0-preview.<date>-<sha>`)
ships the same socket protocol as the release it previews, so it passes. The two
in `e2e/lib.sh` kept comparing the string exactly. The result was not a subtle
drift — it blocked **every** e2e scenario in the repo on a machine running a
preview build, with a message that named the version and read like a real
refusal rather than a stale check.

## Why it went unnoticed

The runtime fix had a test, and it passed. The e2e gate has no unit test by
nature: it runs at the top of a scenario, before the harness exists, and a
scenario that cannot start looks identical to one that has not been run. Nothing
connected the two sites, and the second was only found when a new scenario was
written months later and would not start.

## What to do

When a version, protocol or capability is compared against a pin, grep for the
constant and for the literal before deciding the fix is complete:

```sh
rg -n '0\.9\.0' --glob '!target'      # the literal, not the constant
rg -n 'SUPPORTED_HERDR_VERSION'        # and the constant
```

A rule that reads "accept the release or a preview of it" belongs in one place
every site calls. It is not worth a shared crate for a shell script and a Rust
client, so the next best thing is what this file is for: the sites know about
each other now.

Two of them are fixed: the runtime client and `e2e/lib.sh`, whose rule
`scripts/tests/test_e2e_ci.py` now asserts as "release or preview" rather than
as an exact string — the pinned string was how the gap survived here in the
first place. The eight comparisons in the four real-provider smokes still match
exactly. They are opt-in, outside CI, and refuse loudly rather than passing
wrongly, so they were left alone deliberately; the fix is the same one-line
`case` when someone runs one on a preview build.

## The general shape

This is the same defect class as a guard added to one of two sibling loops. The
signal is not "did I fix the bug" but "how many places implement this rule" —
and the answer is rarely one when the rule crosses a language boundary, because
the compiler cannot see across it.
