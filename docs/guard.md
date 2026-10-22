# Complexity and anti-slop guard for agent edits

The guard checks every edit the model makes. It measures how much the turn grew the code and how complex the touched functions are. It also rejects an edit that leaves a file broken or half written. The guard is off by default in dal and on in dalgona.

Use it to catch two failures early. One is code that grows and gets harder to read each turn. The other is an edit that holds a placeholder instead of code. The guard blocks only invalid content. It reports everything else and leaves the decision to you.

## Turn it on or off

The `[guard]` table in `dal.toml` holds the switch. Turn the guard on in dal:

```toml
[guard]
enabled = true
```

Turn it off in dalgona:

```toml
[guard]
enabled = false
```

With the guard off, none of the reports and rejections on this page appear.

## Read a turn report

When a turn ends, the guard reports from its measurements of the files that turn touched. The report has four groups. The lines below show the shape of each group. Angle brackets stand for values.

**Turn growth.** The `TURN CHANGE SUMMARY.` line counts the lines the turn added and deleted, the net, and the files it touched.

```text
TURN CHANGE SUMMARY. Added <n>, deleted <n>, net <n>, files <n>, new files <n>. This is a report only; no automatic rewrite was started.
```

**Per-file absolute metrics.** Each patch call gives a receipt for each file it changed, up to 20 files. A receipt holds the code lines (`ploc`), the number of functions, and the sums of cognitive and cyclomatic complexity. Further files collapse into `guard: +<n> more files`.

```text
guard: <path> ploc <n> functions <n> cog-sum <n> cc-sum <n>
```

**Per-function metric changes.** A function that crosses a band gets one line with its metric before and after the turn. The metric is `cognitive`, `cyclomatic`, `ploc`, or `nesting`. A file that crosses the file-size band gets a `file` line. `new` stands for a function that did not exist before. The report ranks the crossings and keeps the ten largest.

```text
f <function> <metric> <before>→<after> (over <band>)
file <path> ploc <before>→<after> (over <band>)
```

**Best current.** A `METRICS:` line appears when a turn crosses a band, raises erosion, or churns a file. It gives the files touched, the bands crossed, and the erosion before and after the turn. Erosion is the share of complexity mass that sits in functions with a cyclomatic complexity above 10. A function's mass is its cyclomatic complexity times the square root of its code lines. When erosion rises by the report threshold, the line adds `(human baseline 0.34±0.22)`. Mass lines then rank the functions whose mass grew most. A path that the turn touched at least three times gets a `churn` line. Three is the default of `churn_turn_threshold`.

```text
METRICS: files <n>, bands crossed <n>, erosion <before>→<after>
f <function> mass <before>→<after>
churn <path> touched <n> times this turn
```

Every line names the file or the function and the metric. The report is text for you and for the model. It starts no rewrite.

## Know what the guard rejects

Two edits fail before they land. Each rejection leaves the file unchanged.

- A replacement that is mostly an omission placeholder, such as `// ... rest of the code` or `implementation omitted`, is rejected with `PATCH REJECTED. The replacement contains an omission placeholder. Write the complete replacement text; nothing changed.`
- An edit that leaves a parsed source file with a syntax error, or a `.json` file that does not parse, is rejected. The message reads `EDIT REJECTED. The file is not syntactically valid after this change. No change persists.` It then names the cause and the line. A file that already failed to parse before the edit is exempt.

Three notices only report. They name a fix and change nothing:

- `GUARD-WRAP NOTICE` flags a guard or fallback wrapped around unchanged code.
- `BROAD-HANDLER NOTICE` flags a new catch-all handler.
- `ABSTRACTION NOTICE` flags a new helper with one use.

The helper notice is off until you set `g4_enabled`.

A failed edit or command earns a strike. After three strikes on one repeated target, the guard blocks every further mutating call in that turn. These are edits, commands that write, and evals that act. Reads and pure evals still run. The message begins `Guard stopped this turn after 3 strikes.`

## Tune the thresholds

Set the thresholds under `[guard.policies]`. The bands live under `[guard.policies.bands]`.

```toml
[guard]
enabled = true

[guard.policies]
erosion_report_threshold = 0.1
churn_turn_threshold = 5

[guard.policies.bands]
cognitive = 20
function_ploc = 80
```

| key | default | meaning |
|---|---|---|
| `bands.cognitive` | 15 | the cognitive complexity band for one function |
| `bands.cyclomatic` | 15 | the cyclomatic complexity band for one function |
| `bands.function_ploc` | 50 | the code lines band for one function |
| `bands.nesting` | 4 | the nesting depth band for one function |
| `bands.file_ploc` | 500 | the code lines band for one file |
| `erosion_report_threshold` | 0.05 | the erosion rise that adds the mass lines |
| `churn_turn_threshold` | 3 | the touches per turn that add a `churn` line |
| `g4_enabled` | false | turns on the `ABSTRACTION NOTICE` |

An unknown key fails startup with `unknown guard policy key <key>`.

## Next

- [Settings in dal.toml](config.md) lists the `guard` key beside every other setting.
- [Tools, modes, and approval](tools.md) explains how `patch` and `exec` ask before they run.
