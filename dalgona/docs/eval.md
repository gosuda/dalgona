# eval

`dalgona eval` runs an eval pack: a set of cases, each one a prompt, some seed
files, and a check. It is a shell command, not a model tool, and it is not
the Starlark `eval` tool.

    dalgona eval <pack> [--case <id>] [--repeats <n>] [--model <id>] [--against <dal.toml>]

`<pack>` is a pack name or the path of a `<name>.eval.star` file. A name
resolves to the file `<data root>/evals/<name>.eval.star`; a path loads
directly, so a project can keep its packs beside the code it exercises. No
pack ships with the product: every pack comes from disk.

## Packs

A pack is one Starlark file. It runs under the same dialect as plugins and
sees pure Starlark plus three functions: `evalpack.pack`, `evalpack.case`,
and `evalpack.judge`. It has no `dal.*` namespace, no `ctx`, and no service,
so a pack can only compute values and fail. The file must call
`evalpack.pack` exactly once; top-level code may define functions and
constants.

    def has_shebang(result, workspace):
        return result["exit"] == "ok" and workspace.read("scripts/run.sh").startswith("#!/bin/sh")

    evalpack.pack(name = "edit-anchor-smoke", cases = [
        evalpack.case(
            id = "shebang",
            prompt = "Add a shebang line to scripts/run.sh so it runs as a shell script.",
            files = {"scripts/run.sh": "set -e\necho ok\n"},
            check = has_shebang,
            repeats = 3,
        ),
    ])

`evalpack.case` validates its fields at the call:

- `id`: a non-empty string, unique in the pack.
- `prompt`: 1 to 8000 characters.
- `files`: a dict of relative path to text, at most 64 entries of at most
  65536 bytes each. An absolute path or a `..` is a load error.
- `check`: a function of `(result, workspace)` that returns a bool, or
  `evalpack.judge(question)` with a one-line yes/no question.
- `repeats`: an integer from 1 to 10, default 3.
- `tools`: an optional list of tool names. A tool call outside the list is
  refused with a reason; the tool stays in the model's list, because a root
  session has no per-session tool table to narrow.

`result` is a dict: `exit` (`ok`, `failed`, or `cancelled`), `text` (the
assistant text), `tool_calls` (a list of `{name, args}`), and `usage`
(`{input_tokens, output_tokens, cost_usd}`; `cost_usd` is `None` unless the
provider reported one). `workspace.read(path)` returns a file of the case
workspace as text; a path the run never wrote raises, and the case fails with
the Starlark error line.

## Runs

Each run builds one ephemeral session whose workspace is a fresh temporary
directory seeded with the case files, runs the prompt, applies the check, and
removes the directory. Edits to the case workspace proceed. Every other call
that needs approval, such as `exec`, is refused: a run has no controller to
answer it.
The wall clock of a run is 300 s; a run that exceeds it fails with
`case <id> timed out after 300 s`. After at most 5 s for cancellation, the
run continues. The executor retains the case workspace until cleanup ends.

The model is `--model <id>`, or the `model` of your `dal.toml`. The runner
reads the same configuration as `dalgona`: the product defaults layered with
your `dal.toml`. Cases run in pack order; within a case the arms run in order
and repeats run innermost. `--case <id>` runs one case; `--repeats <n>`
replaces every case's repeat count.

A check that raises fails its case with `<pack>: case "<id>": starlark error
at line <n>: <message>.` and the run continues. A low pass rate is an
observation, never an exit failure.

## Judge-scored cases

A case scored by `evalpack.judge(question)` runs only when the judge gate
resolves on (the `[judge]` table: `auto`, `on`, `off`). With the gate `off`
the case is skipped with `skipped: judge gate is off`; with `auto` and no
usable judge it is skipped with `skipped: judge unresolved`. A skipped case
never counts as failed and its session does not run. The judge sees the
prompt, the assistant text, and the tool calls, and answers a bool.

## Comparison

`--against <dal.toml>` runs every case twice per repeat: once under your
configuration (arm `defaults`) and once with the given file layered on top
(arm `against`). The printed report shows paired deltas, candidate minus
baseline, with `n/a` where an arm has no data. A config that fails to load
fails that arm's runs, not the report.

## Output

The report goes to `<data root>/evals/runs/<pack>/<run id>/report.json`,
and each run appends one line to `runs.jsonl` in the same directory as soon
as it ends. The report is also printed:

    Eval report: <pack> (model <id>, repeats <n>, against <path|none>)
      <case id>  pass <p>/<r>  tokens <in>/<out>  <ms> ms  $<cost|n/a>
    totals: <p> passed, <f> failed, <s> skipped

`report.json` is one JSON object: `v` (1), `pack`, `model`, `against`,
`started_at` (RFC 3339 UTC), and `cases`, a list of `{id, repeats, passed,
failed, skipped, pass_rate, tokens_in_mean, tokens_out_mean, latency_ms_mean,
cost_usd_mean, judge}`. `pass_rate` is `passed / repeats` rounded to three
decimals; a missing mean is `null`. With `--against`, each case has one row
per arm and the row gains an `arm` key. `judge` is `null` for a check case
and `{gate, verdicts}` for a judge case. An interrupted report also carries
`"interrupted": true`.

Exit codes: 0 when a report was written (failures inside cases are
observations, not failures); 1 for a pack load or usage error, with
`<pack>: <reason>` on stderr; 130 on interrupt, with the partial report
written and the remaining runs marked skipped.
