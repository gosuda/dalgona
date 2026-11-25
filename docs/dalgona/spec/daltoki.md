# Daltoki coordinator service

Daltoki runs inside the dalgona daemon as a single owned service. It admits GitHub events, triages and fixes issues through delegation graphs, and holds every decision in a service journal. Disabled configuration MUST perform no credential read, HTTP request, model request, or goroutine start. An empty status MUST remain available. Startup errors MUST fail closed with no partial dispatch.

## Configuration

The `[daltoki]` table carries the keys below. All keys are dalgona-only. `dal` MUST reject the table. The document contract for these keys lives in the config spec.

| Key | Default | Bounds |
|---|---|---|
| `enabled` | `false` | Boolean |
| `app_id` | `0` | Positive when enabled |
| `private_key_file` | `""` | Required when enabled |
| `repositories` | `[]` | 1 to 100 allowlisted entries |
| `poll_interval_seconds` | `60` | 60 to 3600 |
| `webhook_secret_ref` | `""` | Secret-store placeholder when set |
| `relay_url` | `""` | HTTPS without userinfo, query, or fragment |
| `relay_token_ref` | `""` | Secret-store placeholder when set |
| `concurrency` | `2` | 1 to 4 |
| `jobs_per_day` | `20` | 1 to 100 |
| `model` | `""` | Required when enabled |
| `profile` | `"hashline-light"` | Existing home profile |
| `job_token_limit` | `100000` | 1000 to 1000000 |
| `daily_token_limit` | `500000` | At least the job cap, at most 10000000 |
| `job_cost_limit_usd_micros` | `5000000` | Positive |
| `daily_cost_limit_usd_micros` | `20000000` | At least the job limit |

Enabled configuration requires a positive app id, a private key, a model, an existing home profile, and 1 to 100 allowlisted repositories. Repository grammar is `[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+`. Neither component may be `.` or `..`. Names canonicalize to lowercase and reject duplicates.

Secret references MUST name existing secret-store entries. Literal webhook or relay secrets MUST NOT appear in this table. Each nonempty secret-ref value MUST be exactly one recognized placeholder before resolution. Literal, malformed, composite, or unresolved values MUST fail with `daltoki: invalid <key>`. Relay configuration is all-or-none. A partial relay table MUST fail with `daltoki: relay configuration is incomplete`.

## Service lifetime

The service owns concrete `Service`, `New`, `Attach`, `Run`, and `Close` behavior. Battery load constructs the service. `Attach` records the host and starts no worker. The daemon starts Daltoki from a battery callback after `Host.Recover` completes. Recovery failure MUST disable dispatch with `daltoki: host recovery failed`.

`Close` MUST stop intake and dispatch, cancel graphs, and wait up to 10 seconds for owned workers. It MUST preserve service records and unresolved worktrees. Retire and SIGTERM use this same path.

Before cancelling contexts, the service MUST stop publication admission and durably record the shutdown barrier. Queued jobs stay queued. Running graphs settle to their durable terminal or uncertain state. Waiting-approval stays held. Blocked, completed, and failed stay unchanged. Pending and approved drafts keep their decisions but dispatch only after explicit service resume. Sending becomes uncertain and reconciles without resend. Posted and rejected remain terminal. The barrier MUST preserve push intents and poll and relay cursors. At the 10-second timeout the service MUST return an error and keep the service journal lock until workers stop or the process exits. It MUST never allow a replacement worker to dispatch concurrently.

## Job admission

A triage job starts from an open issue. The issue must be created at or after the first service record. A `Bot` author gets triage only when the title starts with the `[dalgona-failure:v1:` marker. A fix job starts from a `labeled` event for the `daltoki` label. Comments and other events create dedup receipts only.

Before admission the service reads the issue events through the bot client. A closed issue admits no job. A fix job needs a current `daltoki` label. The last actor that applied the label MUST have push permission. A failed read, an unknown labeler, or a missing permission admits no job.

The delivery receipt and its job record use one journal append. Each issue gets at most one triage job. Each issue has at most one fix job that is not blocked, completed, or failed. The job id derives from the delivery key, preventing a replayed delivery from creating a second job.

## Dispatch and budgets

Job states are `queued`, `running`, `waiting-approval`, `blocked`, `completed`, `failed`, and `uncertain`. Queued goes to running or blocked. Running goes to waiting-approval, completed, failed, or uncertain. Uncertain goes back to queued. Waiting-approval goes to completed or failed.

One dispatcher starts queued jobs in admission order. At most `concurrency` jobs run at one time. Before a job runs, the dispatcher records a `daltoki.budget.reserved` record for the job token and cost caps on the current UTC day. A requeued job keeps its first reservation. A day holds at most `jobs_per_day` reservations. A day counts the reserved caps of queued, running, and uncertain jobs and the recorded usage of all other jobs. When the next cap does not fit the daily token or cost limit, the job waits while another reservation is held. Otherwise the job becomes blocked with `daltoki: daily job budget exhausted`, `daltoki: daily token budget exhausted`, or `daltoki: daily cost budget exhausted`.

Each job runs in its own git worktree from the default branch of its repository. A triage job runs one read-only node. A fix job runs a read-only `plan` node and then a `fix` node that claims the whole workspace and uses the configured profile. Each node runs as one agent thread with the configured model, and the service records each thread with `daltoki.job.thread`. A change outside the node claims fails the graph. A model without a known price fails the node. The graph stops with `daltoki: job budget exhausted` when the job token or cost cap is spent. The cap includes the usage of earlier interrupted runs.

A triage result becomes a pending comment draft. A fix that changes no file completes the job. A fix that changes a file under `.github/workflows` fails the job. Any other fix becomes a commit on branch `daltoki/<job>` and a pending draft pull request. The service removes a worktree when its job settles in any state other than uncertain.

## Drafts and publication

Draft states are `pending`, `approved`, `rejected`, `sending`, `uncertain`, and `posted`. An edit returns a pending or approved draft to pending. A decision approves or rejects a pending or uncertain draft. A rejection completes its job. Only an approved draft of a waiting-approval job can be sent.

Before a pull request draft is sent, the service records `daltoki.job.push` and pushes the commit with an installation token. The token goes to git as an HTTP header through the environment and never through arguments or URLs. The installation token covers issues, pull requests, and contents. A failed push fails the job.

The service records `daltoki.draft.sending` before each request. Every published body ends with the marker `<!-- daltoki:draft:<id> -->`. A send error records `daltoki.draft.uncertain` and never resends. On restart a sending draft becomes uncertain. The service then looks for the marked comment, or for a pull request from the draft branch. A match records the receipt and completes the job. An uncertain draft without a match waits for a new operator decision. The service checks again for a match before it sends that draft.

After a shutdown barrier, publication stays paused until `Resume` records `daltoki.control.changed` with shutdown false. On restart a running job becomes uncertain and then queued.

## Operator surface

The daemon serves four native RPC methods for the operator. The read method is not effectful. The other three methods are operation extensions. They require `operationId` and keep receipts under the native protocol R07 rules.

| Method | Params | Result |
|---|---|---|
| `dalgona/daltoki/read` | `{}` | `{enabled, running, publicationPaused, lastError, openDrafts, drafts}` |
| `dalgona/daltoki/draft/decide` | `{draftId, expectedRevision, decision, operationId}` | The draft |
| `dalgona/daltoki/draft/edit` | `{draftId, expectedRevision, title?, body, operationId}` | The draft |
| `dalgona/daltoki/resume` | `{operationId}` | `{publicationPaused:false}` |

`drafts` lists at most 100 drafts that are not posted or rejected, oldest first. `openDrafts` counts all of them. A draft has `id`, `jobId`, `kind`, `repository`, `issue`, `title`, `body`, `state`, and `revision`. It has `head`, `base`, `reason`, and `url` only when they are set. The revision is the SHA-256 of the draft state, title, and body. A decision is `approve` or `reject`.

A decision or edit applies only when `expectedRevision` equals the current revision. Otherwise the method returns `stale_revision` with `currentRevision`. So an approval always covers the text that the operator read. An unknown draft returns `not_found`. A state that does not allow the change, an empty body, or a malformed request returns `invalid_params`. A disabled or stopped service returns `unavailable`.

## Journal barrier

The service holds the `daltoki` journal session lock for the coordinator lifetime. It MUST NOT append service records through a thread actor. It MUST NOT mix service records with conversation events.

`Record` carries `Version`, `Kind`, `At`, and optional typed Delivery, Job, Draft, Cursor, Control, and Budget payload pointers. Version MUST be 1 with exactly the payload selected by `Kind`. Event names are `daltoki.delivery.accepted`, `daltoki.cursor.saved`, `daltoki.job.created`, `daltoki.job.state`, `daltoki.job.thread`, `daltoki.job.push`, `daltoki.draft.created`, `daltoki.draft.edited`, `daltoki.draft.decided`, `daltoki.draft.sending`, `daltoki.draft.receipt`, `daltoki.draft.uncertain`, `daltoki.control.changed`, and `daltoki.budget.reserved`. Invalid records or transitions MUST stop dispatch with `daltoki: journal is damaged`.

## Relay intake and installation polling

The relay path has a public `POST /daltoki/webhook` route, Cloudflare secrets `DALTOKI_WEBHOOK_SECRET` and `DALTOKI_RELAY_TOKEN`, and a `DaltokiRelay` Durable Object bound as `DALTOKI_RELAY`. The relay stores verified envelopes for seven days. Its queue is bounded to 10000 items and 64 MiB.

The raw payload limit is 1048576 bytes. Verification MUST use the exact raw bytes with the delivery id, event, and HMAC-SHA256 signature. The envelope wire fields are `deliveryID`, `event`, `receivedAt`, `bodySHA256`, `signature`, and base64 `body`. Envelopes deduplicate transactionally by delivery id and digest. Duplicate same bytes return 202. Different bytes under one id return 409 with `daltoki: delivery id conflict`. A bad signature returns 401 with `daltoki: invalid webhook signature`. A full queue returns 503 with `daltoki: relay queue is full`. An oversize payload returns 413 with `daltoki: webhook exceeds 1048576 bytes`.

Delivery tombstones persist for 30 days. GitHub signs no delivery timestamp, so a first-seen old webhook cannot be proven fresh from a timestamp header. The service MUST state this limitation and MUST refetch current source state before job admission.

Daemon-only bearer routes are `POST /daltoki/relay/pull` accepting `{after, limit 100}` and `POST /daltoki/relay/ack` accepting `{through}`. Pull returns `{items, next}`. A pull response carries at most 786432 bytes of base64 body content. The relay stores each body in segments of at most 96 KiB so no storage value exceeds the Durable Object value limit. The daemon MUST acknowledge only after its intake is durable. It polls relay outbound every 10 seconds with a 15-second timeout. It adds no inbound daemon listener, socket tunnel, or public RPC endpoint. It MUST verify the signature locally again. It MUST reject relay envelopes older than seven days or more than 60 seconds in the future with `daltoki: relay envelope outside replay window`.

Webhook mode reconciles repositories every 900 seconds. Nonrelay mode polls at the configured interval. Polling MUST use installation-compatible repository issues, repository issue comments, and per-issue issue events endpoints. It MUST use `state=all`, `since`, and `per_page=100` where supported. It MUST never use `/notifications` or `/user/issues`.

## Poll watermark

The service persists `PollCursor` with `Since`, `BoundaryIDs`, `ETags`, `Labelled`, `NextURL`, and `ScanStartedAt`. Each endpoint ETag pairs a URL with its ETag. `Labelled` caches each repository's open daltoki-labelled issue numbers. A complete listing MUST refresh that cache and remember the first-page ETag. A listing cut short by the page budget MUST do neither. It MUST leave the repository's events phase incomplete so the next pass resumes it. A 304 on that listing MUST reuse the cached set of the same repository rather than an empty one. A 304 without a cached set MUST retry the first page unconditionally. The service MUST overlap 60 seconds and deduplicate by immutable object id plus update time. It MUST follow validated link URLs. It MUST advance the watermark only after a complete durably admitted scan.

First enable starts at now, never at repository history. Manual runs cover old issues. Each pass handles at most 100 pages and persists continuation without advancing the old watermark. The service keeps at most 1000 LRU ETags and 10000 boundary ids. Boundary overflow MUST hold the cursor with `daltoki: polling boundary exceeds 10000 events`. A completed scan prunes boundary ids to entries the scan still observed and refreshes the scan start watermark. It persists each repository's open-issue numbers so a cached issues response keeps the scan target. Polling honors the largest `Retry-After`, `X-Poll-Interval`, and rate-limit reset delay each pass reports.

The service MUST honor ETag and 304 responses and wait for the maximum of the configured interval, `X-Poll-Interval`, and the rate deadline. A failed or incomplete scan MUST never advance the watermark. A completed empty scan advances to its recorded start time. Current-state fetch failures admit no job. Label attribution failure authorizes no fix. Ignored events create dedup receipts only, with no reply, reaction, label, or model request.
