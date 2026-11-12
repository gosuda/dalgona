# Ratchet

The ratchet gate judges one variant against the best variant so far. This page states its statistic, decision and stop rules.

## Statistic

The gate MUST pair each case of a candidate with the same case of the reference variant.

The gate MUST take the candidate case mean minus the reference case mean for each paired case.

The gate MUST report the mean of the paired differences and a two-sided 95 percent Student t half-width.

The half-width MUST use the critical value for n minus 1 degrees of freedom, and 1.96 above 30 degrees of freedom.

The gate MUST return `up` when the gain minus the half-width is above zero.

The gate MUST return `down` when the gain plus the half-width is below zero.

The gate MUST return `flat` in every other case, and when fewer than two values exist.

For a metric where a lower value is better, the gain MUST be the negated mean.

## Decision

A guardrail whose train or test verdict is `down` MUST revert the round.

A target whose train or test verdict is `down` MUST revert the round.

A target whose train and test verdicts are both `up` MUST keep the round.

A target whose train verdict is `up` and whose test verdict is not `up` MUST revert the round as an overfit suspect.

A target whose test verdict is `up` and whose train verdict is not `up` MUST rerun the round.

Every other round MUST revert as a change outside the noise.

A variant MUST NOT be rerun more than two times. A third rerun decision MUST revert.

## Stop

The flow MUST stop when the reverts since the last keep reach the plateau limit.

The flow MUST stop when the count of gated variant rounds reaches the round limit.

A keep or a baseline round MUST reset the count of reverts since the last keep to zero.

A rerun round MUST leave the count of reverts since the last keep unchanged.

The flow MUST NOT stop while its last round is a rerun.

## Flow directory

A flow MUST keep its state in `.dalgona/ratchet/<flow>/state.json` under the workspace.

A flow name MUST match `^[a-z0-9][a-z0-9_-]{0,63}$`.

The host MUST read and write flow files through an `os.Root` opened at the workspace.

The host MUST write `state.json` through a temporary file and a rename.

A case, harness, change, or off-limits path MUST stay outside `.dalgona/ratchet`.

The gate MUST detect a changed case file, harness file, or plan. The gate MUST NOT protect `state.json` against a change by the model.

## Setup

`init` MUST refuse an empty list of case, harness, or change paths.

Every case, harness, and change path MUST name an existing file or directory inside the workspace.

An off-limits path MUST stay inside the workspace.

A change path MUST NOT overlap a case, harness, or off-limits path. Two paths overlap when one equals the other or names a directory that contains it.

`plan` MUST refuse a hold list of more than 8 metrics, or a hold list that includes the target.

`plan` MUST refuse reps outside 1 to 20, and MUST refuse fewer reps after a baseline round.

`plan` MUST refuse a plateau outside 2 to 10, or a round limit outside 1 to 50.

A runner command MUST contain `{variant}`.

A metric MUST default to `lower` for `latency_s` and `cost_usd`, and to `higher` otherwise.

`plan` MUST default reps to 2, plateau to 3, and rounds to 10 when the flow has no value.

## Split

`split` MUST refuse fewer than 4 cases, and MUST refuse a test fraction outside 0.1 to 0.9.

`split` MUST refuse a case id that does not match `[A-Za-z0-9][A-Za-z0-9._-]{0,127}`.

`split` MUST refuse a call after the baseline round is gated.

`split` MUST assign each stratum to test by rounding its size times the test fraction.

`split` MUST move one case when the train side or the test side is empty.

The same seed and strata MUST give the same train and test ids.

## Approvals

An approval MUST bind the digest of its stage at the time of approval.

The inputs digest MUST cover every case path, and the grader digest MUST cover every harness path.

The plan digest MUST cover the goal, the reps, the stop rule, the command, the change and off-limits paths, and the split.

A directory digest MUST list each regular file by its slash path and its content.

A digest MUST refuse a directory that holds more than 10000 files.

A missing path MUST contribute its name to the digest, marked as missing.

An approval MUST report `stale` when its digest differs from the current digest, and `missing` when none exists.

## Runner contract

A variant MUST write `results.jsonl` in its output directory, with one row for each case and repetition.

A row MUST carry `case_id`, `rep`, and `status`. The `status` MUST be `ok` or `truncated`.

A row MUST carry `grade`, which MUST be a number, a boolean, or an object of numbers and booleans.

A failed attempt MUST go to `errors.jsonl` with `case_id`, `rep`, and a non-empty `class`.

A train case MAY write a trace named `traces/<case>_rep<k>.json`. A test case MUST NOT write a trace.

A test row MUST NOT carry text fields other than `case_id`, `status`, `model`, and `tags`.

## Gate checks

The gate MUST refuse a variant that lacks a `(case, rep)` slot, and MUST name the missing slots.

The gate MUST refuse a variant with a duplicate `(case, rep)` row.

A slot that only errored MUST make the variant rerun, except in the baseline, which MUST refuse.

The gate MUST refuse a trace of a test case, and any trace file that is not `<case>_rep<k>.json`.

The gate MUST refuse a test row that carries output text.

The gate MUST refuse a guardrail with fewer than two measured cases on train or test.

The gate MUST refuse a change to the change paths made after `check`.

The gate MUST replay a round whose results digest is unchanged, and MUST NOT record a second round.

The `check` action MUST refuse a variant that is not the next expected variant.

The `check` action MUST refuse a non-baseline variant whose change digest equals the best round's.

The `check` action MUST refuse while an approval is not current, or while the flow is done.

The baseline gate MUST warn when the target sits near its ceiling, or when train and test differ by more than their intervals.

## Train view

The train view MUST list the rows and traces of train cases only.

The train view MUST NOT list a row or a trace of a test case.

The train view MUST give each train case its target mean for every gated variant, or none when the variant lacks it.

The status table MUST mark the best round with `(best)`.

## Tool

The `ratchet` tool MUST run only on the local runner. It MUST refuse any other runner.

Each call MUST name one action. The action MUST be `init`, `plan`, `split`, `approve`, `check`, `gate`, `train`, or `status`.

The tool MUST refuse a member that the action does not take, and MUST name the member.

Calls on one flow MUST run one at a time. The `approve` action MUST wait for the user without holding that lock.

The `approve` action MUST ask about the digest of its stage, and MUST record an approval only when that digest is still the same.

The `approve` action MUST return `asked` false, and MUST NOT ask, when its stage is already current.

The `approve` action MUST return `declined: ratchet approvals need an interactive client` when no client is attached.

The `approve` action MUST return `cancelled: the question was closed; nothing was recorded` when the user cancels.

The `approve` action MUST record nothing on abort, and MUST return the feedback text on revise.

The `split` action MUST derive its seed from the call id when the call gives no seed.

The `gate` action MUST require a summary of at most 200 characters, except for the baseline.

The `status` action MUST return the approvals, the best round, the done state, and the round table.

The tool MUST accept the arguments of the schema `schemaJSON` in `internal/ratchet/tool.go`.

The tool MUST accept the `action` and `flow` members for every action.

The tool MUST return JSON text with the keys of the table below for each action.

| Action | Members it takes | Keys of its result |
|---|---|---|
| `init` | `cases`, `harness`, `change`, `off_limits` | `cases`, `change`, `dir`, `flow`, `harness`, `off_limits` |
| `plan` | `target`, `direction`, `hold`, `directions`, `reps`, `plateau`, `rounds`, `command` | `command`, `goal`, `reps`, `stop` |
| `split` | `strata`, `test_fraction`, `seed` | `seed`, `strata`, `test`, `train` |
| `approve` | `stage`, `question`, `preview` | `approved`, `asked`, `sha`, `stage` on approval |
| `approve` (abort) | `stage`, `question`, `preview` | `aborted`, `approved`, `asked`, `stage` |
| `approve` (revise) | `stage`, `question`, `preview` | `approved`, `asked`, `feedback`, `stage` |
| `check` | `variant` | `changed_files`, `command`, `expected_rows`, `out_dir`, `reps`, `variant` |
| `gate` | `variant`, `summary` | `best`, `changed_files`, `decision`, `deltas`, `done`, `done_reason`, `flat_rounds`, `plateau`, `reasons`, `table`, `test`, `train`, `variant`, `warnings` |
| `train` | `variant` | `history`, `rows`, `traces`, `variant` |
| `status` | none | `approvals`, `best`, `done`, `done_reason`, `flat_rounds`, `flow`, `plateau`, `table` |
