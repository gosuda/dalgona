# Rules: stream, always, and rulebook

Three kinds: stream rules watch output, always rules apply every turn, and rulebook files hold project rules. User rule files live under `<data-root>/rules/`; project rule files live under `<workspace>/.dal/rules/`. Enabled plugins can also supply files from their `rules/` directories. Read a rule at `rule://<name>`.

## Interrupts and retries

Stream rules can interrupt a response and supply a reminder for the retry. `rules.max_retries` caps these interrupts per turn. At the cap, unjudged matches supply reminders without interrupting. Judged matches become report-only: they do not call the judge or add a reminder. A rule marked as report-only never interrupts.

Rule reminders are written to the session journal before delivery. They enter model context and its token estimate, and remain available when you resume the session. A session keeps its captured rule set; editing a rule file does not change an already-open session.

## Judged rules

A judged rule has a pattern and a yes-or-no judge question. Session startup checks the selected judge route and its credentials without generating a response or consuming a scripted response. Readiness does not guarantee that a later model request will succeed.

`rules.judge` controls which judged rules enter the session's matching set:

- `off` excludes judged rules.
- `auto` includes them when the shared judge is ready. Otherwise, it excludes them and reports a readiness note.
- `on` requires a ready shared judge. If none is ready, session startup fails.

The shared judge has a separate `gate` setting under `[judge]`. Setting `judge.gate = "off"` skips its readiness check. Setting `rules.judge = "off"` does not disable the shared judge, which other features can use. With either gate set to `on`, an unavailable judge prevents startup. Unexpected readiness errors are returned, not treated as missing credentials.

When a judged match can interrupt, dal closes the provider stream before requesting the verdict:

- `true` confirms a violation. dal records the rule fire and retries with its reminder.
- `false` rejects the match. dal reports one informational pass note and retries without a rule fire or reminder.
- A judge error or timeout reports one informational failure note and retries without a rule fire or reminder.

The rule is latched for that turn before the verdict wait, so a retry does not judge the same rule again. Cancellation stops the wait without a late rule fire or retry. The configured retry cap still applies.

See [Settings in dal.toml](config.md) for rule defaults.
