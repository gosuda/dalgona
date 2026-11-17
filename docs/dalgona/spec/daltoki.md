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
| `profile` | `"daltoki"` | Existing home profile |
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

## Journal barrier

The service holds the `daltoki` journal session lock for the coordinator lifetime. It MUST NOT append service records through a thread actor. It MUST NOT mix service records with conversation events.

`Record` carries `Version`, `Kind`, `At`, and optional typed Delivery, Job, Draft, Cursor, Control, and Budget payload pointers. Version MUST be 1 with exactly the payload selected by `Kind`. Event names are `daltoki.delivery.accepted`, `daltoki.cursor.saved`, `daltoki.job.created`, `daltoki.job.state`, `daltoki.job.thread`, `daltoki.job.push`, `daltoki.draft.created`, `daltoki.draft.edited`, `daltoki.draft.decided`, `daltoki.draft.sending`, `daltoki.draft.receipt`, `daltoki.draft.uncertain`, `daltoki.control.changed`, and `daltoki.budget.reserved`. Invalid records or transitions MUST stop dispatch with `daltoki: journal is damaged`.

## Relay intake and installation polling

The relay path has a public `POST /daltoki/webhook` route, Cloudflare secrets `DALTOKI_WEBHOOK_SECRET` and `DALTOKI_RELAY_TOKEN`, and a `DaltokiRelay` Durable Object bound as `DALTOKI_RELAY`. The relay stores verified envelopes for seven days. Its queue is bounded to 10000 items and 64 MiB.

The raw payload limit is 1048576 bytes. Verification MUST use the exact raw bytes with the delivery id, event, and HMAC-SHA256 signature. The envelope wire fields are `deliveryID`, `event`, `receivedAt`, `bodySHA256`, `signature`, and base64 `body`. Envelopes deduplicate transactionally by delivery id and digest. Duplicate same bytes return 202. Different bytes under one id return 409 with `daltoki: delivery id conflict`. A bad signature returns 401 with `daltoki: invalid webhook signature`. A full queue returns 503 with `daltoki: relay queue is full`. An oversize payload returns 413 with `daltoki: webhook exceeds 1048576 bytes`.

Delivery tombstones persist for 30 days. GitHub signs no delivery timestamp, so a first-seen old webhook cannot be proven fresh from a timestamp header. The service MUST state this limitation and MUST refetch current source state before job admission.

Daemon-only bearer routes are `POST /daltoki/relay/pull` accepting `{after, limit 100}` and `POST /daltoki/relay/ack` accepting `{through}`. Pull returns `{items, next}`. The daemon MUST acknowledge only after its intake is durable. It polls relay outbound every 10 seconds with a 15-second timeout. It adds no inbound daemon listener, socket tunnel, or public RPC endpoint. It MUST verify the signature locally again. It MUST reject relay envelopes older than seven days or more than 60 seconds in the future with `daltoki: relay envelope outside replay window`.

Webhook mode reconciles repositories every 900 seconds. Nonrelay mode polls at the configured interval. Polling MUST use installation-compatible repository issues, repository issue comments, and per-issue issue events endpoints. It MUST use `state=all`, `since`, and `per_page=100` where supported. It MUST never use `/notifications` or `/user/issues`.

## Poll watermark

The service persists `PollCursor` with `Since`, `BoundaryIDs`, `ETags`, `NextURL`, and `ScanStartedAt`. Each endpoint ETag pairs a URL with its ETag. The service MUST overlap 60 seconds and deduplicate by immutable object id plus update time. It MUST follow validated link URLs. It MUST advance the watermark only after a complete durably admitted scan.

First enable starts at now, never at repository history. Manual runs cover old issues. Each pass handles at most 100 pages and persists continuation without advancing the old watermark. The service keeps at most 1000 LRU ETags and 10000 boundary ids. Boundary overflow MUST hold the cursor with `daltoki: polling boundary exceeds 10000 events`.

The service MUST honor ETag and 304 responses and wait for the maximum of the configured interval, `X-Poll-Interval`, and the rate deadline. A failed or incomplete scan MUST never advance the watermark. A completed empty scan advances to its recorded start time. Current-state fetch failures admit no job. Label attribution failure authorizes no fix. Ignored events create dedup receipts only, with no reply, reaction, label, or model request.
