# Consented failure reporter

The reporter files bounded failure reports to `gosuda/dalgona` through the shared GitHub client. It uses the `reporter` policy only: a 10-second timeout, a 1 MiB cap, no redirects, and no automatic retry. Nothing is filed without explicit person consent. Local logging and manual reports work without consent.

## Consent gate

Reporting is controlled by `[failure_reports]` with `enabled` and `auth`. Both keys are dalgona-only and user-home-only. `auth` accepts exactly `app` and `gh` and defaults to `app`. Invalid values MUST fail with `config: failure_reports.auth must be one of app, gh`. Model, project, or environment configuration MUST NOT enable reporting or switch authentication.

Opt-in shows the exact substituted consent text naming the product, the included fields, the excluded fields, and the selected method. Only an exact terminal `yes` line opts in. Any other text, EOF, or no terminal prints `automatic failure reporting remains off`. No `--yes` bypass exists.

Consent persists schema-1 JSON with UTC time, the selected method, and the SHA-256 of the substituted text in `<home>/failure-reporting/consent.json`, then enables config. A write error leaves reporting off. Enabled config with true value but no consent prints `automatic failure reporting is off: consent is required`.

Changing `auth` invalidates consent for remote work. It MUST print `automatic failure reporting is off: authentication changed; run dalgona failures reporting on`. It preserves queue, receipts, caps, and latches. Fresh consent is required before the new method is used. Broad `gh` authorization MUST NOT inherit narrower App consent.

`Off` disables before more work, removes consent and the App credential, and deletes queued excerpts. It preserves local logs, dedup receipts, counters, and restriction deadlines. It prints `automatic failure reporting disabled; queued reports deleted` and `published GitHub issues are not deleted`.

## Authentication

App mode uses a separately registered public reporting GitHub App with Issues write only on `gosuda/dalgona`. It MUST NOT use the Daltoki installation identity. Gh mode uses only the active `github.com` credential from `gh auth token --hostname github.com --no-refresh`. It MUST NOT read `GH_TOKEN`, `GITHUB_TOKEN`, inferred API-key variables, or provider credentials. It MUST NOT run `gh auth login`, `gh auth refresh`, or any issue command.

Failure of one method MUST never select the other. A 401, 403, or 429 restriction persists across authentication changes. Configuring `gh` MUST NOT bypass an App restriction.

The release supplies the public App client id at build time through `DALGONA_REPORT_CLIENT_ID`. The build uses a generated overlay with an immutable const. No runtime environment lookup, mutable global, or init is added. An absent client id makes App reporting unavailable with exactly `failure reporting is unavailable: github app client id is missing`. Explicitly configured `gh` mode needs no App client id.

App authorization starts on reporting-on or login only, with a terminal. It resolves repository metadata and requires positive id and full name `gosuda/dalgona`. Device polling uses provider id `failure-reporting`, the device and token URLs, `offline_access` scope only, and decimal repository id on every poll. Poll display, interval floor, and one-flow-per-home rules follow the OAuth device flow. Token expiry keeps consent but pauses with `failure reporting authorization expired; run dalgona failures reporting login`.

Gh acquisition shows `using the active github.com account from gh; no login, refresh or fallback will run`. It invokes the `gh` executable directly with a five-second deadline and capped output. It strips one terminal line ending and rejects empty, embedded-control, or oversized output. It removes token-bearing variables from the child environment and redirects stdin to null. It obtains at most one token per drain run and caches it only for that run. After an HTTP restriction it launches no more subprocesses until the person clears the latch.

Production rejects testing mode, explicit test mode, and every overridden product home. An alternate home MUST NOT acquire credentials, authorize the App, or send metadata. It prints `automatic failure reporting is off for an overridden home`.

## Masking

Every report renders through the shared masked rendering of the Failure record. Reports include the product version, OS, architecture, failure class, tool name, and fingerprint. They include the masked error and stack plus a bounded masked excerpt of tool arguments and output. They exclude prompts, conversation history, and absolute paths. The public step data comes only from the capture-time allowlisted failing step, never a full local case or file blob.

## Reporter policy and bounds

The reporter constructs its client with base URL `https://api.github.com`, one repository entry for `gosuda/dalgona`, and policy `reporter`. It adapts its credential source through the user credential with expiry rejection before dispatch.

Only post-consent dalgona main-origin reportable records enqueue. The reporter MUST never enqueue `dal`, side, doctor, test, or reporter errors. It MUST NOT replay pre-consent history. Queue limits are 64 fingerprints, 1 MiB encoded, and seven days. Overflow keeps existing data, drops the record, and returns `failure reporting queue full; report kept locally`. Receipt states are `attempted`, `filed`, `found`, and `uncertain`. The reporter reserves `attempted` before creation. It MUST stop at 100000 receipts or 16 MiB with `failure reporting stopped: deduplication state is full`. Damaged state MUST stop with `failure reporting stopped: state is invalid`.

Search covers all states with `repo:gosuda/dalgona is:issue in:title "dalgona-failure:v1:<fingerprint>"`, page size 100, at most ten pages. Results MUST require the exact full title marker and repository and MUST exclude pull requests. An exact match selects the smallest issue number and stores `found`. A recurrence MAY send one `eyes` reaction per fingerprint per home, with the attempt reserved before POST. HTTP 200 or 201 completes creation. Ambiguity MUST NOT retry. Later recurrences change only local counters.

Durable daily caps per UTC day and home are three issue-create attempts, three reaction attempts, and twenty REST requests. Run caps are one create, one reaction, and ten REST requests. Writes serialize with 60 seconds between any issue or reaction writes across restarts. Exhaustion retains work with `failure reporting paused: local limit reached`. The reporter tracks core and search buckets separately and uses the latest of reset, `Retry-After`, and a 60-second floor.

Any 401, 403, or 429 persists a stop latch with `failure reporting stopped after HTTP <status>; review access before enabling again`. No retry, refresh, source switch, new acquisition, login, restart, or authentication change bypasses it. HTTP 404, 410, or 422 stops with `failure reporting stopped after HTTP <status>; check repository access and issue policy`. Search transport and 5xx errors wait for a later run. POST transport, 5xx, or invalid responses become `uncertain` with no second attempt.

Title and body follow the shared rendering: `[dalgona-failure:v1:<fingerprint>] <product> <version> <class> <tool-or-runtime>` within 240 UTF-8 bytes, and a 32768-byte body with four-space indents and escaped markup. POST sends title and body only, never labels, assignees, milestone, project, or type. Daltoki recognizes the title marker for triage routing only. The marker proves no authorship or write permission.
