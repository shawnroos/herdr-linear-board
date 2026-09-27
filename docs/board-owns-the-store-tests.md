# Work plugin test inventory

What the work plugin's bats suite pins today, and what each part becomes if the board owns the work
store ([board-owns-the-store.md](board-owns-the-store.md)). It prices the move; it is not a porting
plan.

All facts are against shrimpshack `origin/main` at `223f27c` (`plugins/work/tests/`). The installed
`work` 0.5.0 plugin cache is an older build with the same version string, so it was not used.

## Size

- 38 bats files, 18,582 lines, 1,303 tests.
- About 3,050 of those lines (16%) are setup and inline stubs before the first test.
- Plus `tests/unit/setup_common.bash` (58 lines, loaded by every suite) and `tests/run-tests.sh`
  (912 lines, 16 static gates).
- Roughly **900 to 1,000 behaviours** are worth porting. The Rust suite does not need to match the
  line count: Rust fakes replace the curl-script and fake-CLI setup.

## Behaviour classes

| Class | Files | Lines / setup / tests | Verdict |
|---|---|---|---|
| Placement engine | `board-plan` | 506 / 106 / 28 | Moves as-is |
| Config validation | `board-config` | 712 / 54 / 62 | Moves, about 90% as-is |
| Store semantics | `binding`, `board-store`, `repos`, `context-filter`, `states` | 3,315 / 267 / 252 | Invariants move; file mechanics go |
| Linear client, writes, consent | `linear`, `board-linear`, `reconcile`, `description`, `documents`, `views`, `create`, `start`, `propose` | 5,432 / 712 / 385 | Invariants move; needs a Rust fake Linear |
| Credential and fakes | `fake-linear`, `migrate`, `secrets` | 1,144 / 200 / 81 | Mostly obsolete or re-scoped |
| herdr sync and placement | `board-sync`, `board-write`, `board-attended`, `board-herdr`, `placement`, `herdr-read`, `herdr-write` | 4,062 / 1,028 / 235 | Behaviours move onto boardd's herdr stack |
| Hooks | `ground`, `board-behind` | 580 / 121 / 39 | Stay bash, get thin |
| Snapshot wire and bin envelopes | `snapshot`, `spaces`, `projects`, `issue-detail` | 1,187 / 321 / 79 | Obsolete as a wire |
| Bash-only gates | `isolation`, `wire`, `settings-doc` | 544 / 95 / 45 | Obsolete; two ideas survive |
| Paths and names | `contain`, `schemes`, `worktree-remove` | 1,100 / 148 / 97 | Moves as-is |

| Verdict | Classes | Lines | Tests |
|---|---|---|---|
| Port as-is | placement engine, config validation (most), paths and names | about 2,300 | about 180 |
| Port the invariant, new harness | store, Linear writes, herdr sync, hook content | about 13,400 | about 910 |
| Obsolete | snapshot wire, bash-only gates, most credential and fake tests, plus the file-mode, argv, sourcing, binary-resolution and lock slices of the others | about 2,900 plus slices | about 270 |

## Per class

**Placement engine.** Pins the pure classifier: agreement, move, write-back candidate,
conflict/close/move questions, hide and unhide, pointer relink, which space is a ticket's home when
the global mapping and an override both claim it, and "an incomplete read never classifies a ticket
as leaving". It already takes one JSON in and gives one JSON out (`lib/board-plan.sh:4-6`), so it
ports to a pure function beside `board-core::engine`. The six `tests/fixtures/board/*.json` files
become table tests.

**Config validation.** Pins default-deny validation, the resolver (a space mapping replaces the
global one), the triage/backlog exclusion, `level-of`, and the session scope clause. About six tests
cover file mode, symlinks and the lock; they go if the config lives in SQLite. "Space order in the
file decides a ticket's home" needs an explicit order column.

**Store semantics.** The domain rules move: the binding state machine and its nonce ordering (a
superseded nonce is dead, a declined candidate never returns), a branch change downgrading a binding
to proposed, `prior_bindings` on rebind, consent covering team, project and branch, question
preconditions, per-field space consent, sync `behind`, session isolation (two herdr sessions with the
same workspace id do not share a record), and misplaced/stale suspension of writes. The per-file
mode, truncation, rename and lock tests go. One new test replaces them: importing the existing JSON
store into SQLite once.

**Linear client, writes, consent.** The rules move: branch-to-identifier parsing, error mapping and
retry, paging with truncation flags, the cache freshness bound, `write_allowed`, shadow mode
(compute, log, send nothing), the consent gate, the GraphQL shape of every mutation, description
template validation, document create-then-update, and `start`'s path, branch and repository
question with idempotent retry. The "credential never reaches argv" tests are specific to a curl
subprocess and collapse to "the key is never logged". This class needs a Rust HTTP fake Linear; it
can reuse the plugin fake's captured response shapes, not its role as a curl stand-in.

**Credential and fakes.** Keychain access from Rust needs a much smaller suite; the `security -w`
traps are specific to the CLI. `migrate` stays only if the plaintext fallback stays.
`fake-linear.bats` tests the fake itself and is replaced by whatever guards the Rust fake has.

**herdr sync and placement.** This is the plugin's own herdr driver, and it overlaps what boardd
already owns (`board-herdr`, `spawner/placement/`, `watchers/`, `herdr_conn.rs`, `testkit.rs`).
Keep the behaviours as boardd tests on its existing fake-Herdr builders: crash-safe tab rebuild,
in-use pane rules, held panes, swaps through parking, tab and pane caps, write-back only through
the gate, and where a session opens. The binary-resolution and pid-file lock tests go once one
daemon owns the sync. This class carries the most setup: 1,028 of its 4,062 lines are stubs.

**Hooks.** They stay Claude Code hook scripts and shrink to `board` calls. The context wrapper and
sanitising tests move to wherever the text is produced. "Exits 0 on every failure" stays as a thin
shell test.

**Snapshot wire and bin envelopes.** These exist because boardd runs bash scripts. The envelope,
exit-code, cleared-env and cross-repo hash-pin tests go with the wire. The content rules move into
boardd's Linear-mode tests: view fallback to team columns, archived and not-in-project views,
unmapped tabs, offline project naming, and sanitising at range edges.

**Bash-only gates.** Each exists because bash has no modules or types, and all go. Two ideas
survive. "Only a person-facing entry point can confirm" becomes a Rust test that no MCP tool or
non-interactive CLI verb reaches the confirm path. The settings-doc check already has a home in
`scripts/tests/test_docs.py`.

**Paths and names.** Pure logic that becomes table tests: root containment (symlink, hardlink,
sibling prefix), worktrees-root rules, naming schemes and their length caps, and safe worktree
removal (commits delivered, no process running in it, the answered question's nonce required).
Removal needs real git repositories, which the crate tests already build, plus seams for `gh` and
`lsof`.

## Test fakes

| Fake | Lines | Stands in for | Becomes |
|---|---|---|---|
| `tests/fixtures/fake-linear.sh` | 1,213 | curl, not the API | A Rust HTTP fake reusing its captured shapes |
| `tests/fixtures/fake-herdr.sh` | 431 | the herdr CLI | boardd's existing fake-Herdr builders |
| `tests/fixtures/fake-herdr-socket.py` | 599 | the herdr socket | boardd's existing fake-Herdr builders |
| `tests/fixtures/fake-security.sh` | 196 | `/usr/bin/security` | A small Keychain seam, or nothing with a native API |

The two herdr fakes disagree. `fake-herdr.sh` reports herdr 0.8.2 / protocol 20 in its status
output but 0.9.0 / protocol 22 in its canned snapshot; `fake-herdr-socket.py` pins 0.9.0 /
protocol 22. boardd accepts only 0.9.0 / protocol 22.
