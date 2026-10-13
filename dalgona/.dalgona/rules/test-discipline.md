---
description: Test discipline for every change that touches a test in this repo.
globs:
  - "**/tests/**/*.rs"
  - "**/test/**/*.rs"
  - "**/test_support/**/*.rs"
  - "**/benches/**/*.rs"
---
# Test discipline

Every test in this repo must pass the workspace gates in one run: `cargo fmt --check`, `cargo clippy --all-targets --all-features --locked -- -D warnings`, `cargo test`. A test that needs a filter, its own process, a retry, or a run order to pass is broken. Fix the test; do not pamper it.

## Flaky equals failing

A test that passes 9 of 10 times is failing 10 percent of the time. Do not sleep in a test body unless time itself is the system under test. "Wait long enough" is a guess, and the guess fails on another machine. Subscribe before the trigger, and await the signal with an explicit timeout that names what it waited for, for example `waited 5s for event 'X', never fired`. The timeout is a circuit breaker, never a synchronization primitive.

## No isolation crutches

`#[ignore]` to mask a flaky test, a dedicated process to mask a state leak, and a reorder that hides cross-test contamination are all banned. Cross-test contamination is a state leak: reset in the fixture, or move the shared state behind one owner. Tests must pass under any order the runner picks.

## Assert behavior, not prose

A prompt, rule, or instruction file is prose. Its wording is not a contract. Do not pin phrases, snapshots, or word counts of prose. Decide by what consumes the file. A machine that reads a value from it gets a test of that value. Two shipped copies that must stay identical get one equality test between the real artifacts. Pure prose with no machine consumer gets no automated test; review is the guard, and a green text-pin here is pretend-coverage. When a behavioral seam exists, test the branch the code enforces, keyed on a stable token the runtime also uses.

## Delegation

When you hand test writing to a subagent, give it the behavior the test must distinguish, never a ready-made assertion string or an expected count. A prescribed mechanism that is wrong gets implemented faithfully, and the defect ships behind a green suite.
