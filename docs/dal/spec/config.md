# Configuration

The program MUST read `config.toml` and then `settings.toml` from its product home. Settings MUST override config values. Command flags MUST override both files. Defaults MUST apply when no layer sets a value.

`dal` uses `$DAL_HOME` or `~/.dal`. `dalgona` uses `$DALGONA_HOME` or `~/.dalgona`. The loader MUST expose the resolved home. The resolved home MUST be an absolute path. A relative home MUST resolve against the directory the process started in. The two homes MUST remain separate. On Windows the `dal` host start and the dalgona daemon start MUST reset the protected DACL on the product home. The same reset MUST apply to every file and directory inside the home. The same reset MUST NOT change a symlink or an irregular entry.

The loader MUST reject unknown keys. The error MUST report the file and its 1-based line. `dal` MUST reject `runner`, `remote`, `runners`, `remotes`, `mcp_servers`, `mcp_trusted_workspaces`, `discovery`, `herdr`, `web`, `edit_review`, and `daltoki` as dalgona-only settings. Presence MUST trigger this rejection, including an empty table or a false value. `tool_mode` and `[eval]` MUST load in both products.

If a layer cannot be read or fails validation, `Load` MUST return an error.

## Shell environment

`shell_env` defaults to `snapshot` in both products.
`inherit` disables login-shell capture and uses the current daemon environment.
Other values MUST fail with `<file>:<line>: shell_env must be one of snapshot, inherit`.

## MCP HTTP authentication

Each `[mcp_servers.<name>]` table MAY set these dalgona-only keys.

| Key | Default | Meaning |
|---|---|---|
| `bearer_token_env` | `""` | Environment variable that holds a bearer token |
| `oauth` | `false` | Enable resource-bound OAuth |
| `oauth_scopes` | `[]` | Requested scopes instead of resource metadata scopes |
| `oauth_client_id` | `""` | Registered client identifier instead of dynamic registration |
| `oauth_callback_port` | `54871` | Loopback callback port |

Authentication keys require an HTTP URL.
`bearer_token_env` and `oauth` exclude each other.
The environment variable name MUST use letters, digits, and underscores, without a leading digit.
OAuth server names MUST contain 1 to 32 lowercase letters, digits, underscores, or hyphens.
OAuth URLs MUST use HTTPS, except on loopback hosts.
The callback port MUST be between 1024 and 65535.
Validation errors MUST include the file, line, server table, and invalid key.

## Core defaults

The default approval mode is `auto`. The default sandbox is `workspace-write`. The default tool mode is `eval-first` for both products. `tool_mode = "normal"` restores the full tool list. `tool_mode = "eval-only"` limits model-visible tools to `ask`, `yield`, and `eval`. The default runner is `local`. The default reasoning level is `default`.

When no layer sets `edit_style`, every model MUST use the edit style `anchor`.

`[limits].max_subagent_starts` MUST be a positive integer.
Its default is 32 starts per root turn.
A zero or negative value MUST fail configuration loading with the file and line.

The approval mode MUST be the top-level scalar `approval`. Its allowed values are `auto`, `yolo`, and `ask`. The decider settings MUST use `[approval_decider]`:

```toml
approval = "auto"

[approval_decider]
decider = "decisions:typesafe/jev-1.14"
threshold = 0.9
```

`approval_cfg` and `[approval]` are not valid configuration keys.

The decider MUST start with `decisions:` or `llm:`. The threshold MUST be in `(0, 1]`. Validation errors MUST name `[approval_decider] decider` or `[approval_decider] threshold`.

The `[edit_review]` table MUST set the opt-in edit review, and it MUST be dalgona only. Its key `enabled` MUST be a boolean, and it defaults to `false`.

```toml
[edit_review]
enabled = true
feedback = ["lost_code", "placeholder", "incomplete"]
```

An absent `feedback` MUST select the built-in speaker set. An empty `feedback` MUST select no speaker. The `sound` question MUST NOT be a speaker.

## Daltoki coordinator settings

The `[daltoki]` table configures the Daltoki coordinator service. It MUST be dalgona-only. `dal` MUST reject it as a dalgona setting, including an empty table or a false value. The rejection error MUST read `<file>:<line>: daltoki is a dalgona setting`. Daltoki behavior is specified in the dalgona Daltoki topic.

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `false` | Start the coordinator in the daemon |
| `app_id` | `0` | GitHub App numeric id |
| `private_key_file` | `""` | Path to the App RSA private key |
| `repositories` | `[]` | Allowlisted `owner/name` entries |
| `poll_interval_seconds` | `60` | Nonrelay poll interval in seconds |
| `webhook_secret_ref` | `""` | Secret-store reference for the relay webhook |
| `relay_url` | `""` | Relay base URL |
| `relay_token_ref` | `""` | Secret-store reference for the relay bearer token |
| `concurrency` | `2` | Maximum concurrent owned graphs |
| `jobs_per_day` | `20` | Admission cap per UTC day |
| `model` | `""` | Model used for triage and fix graphs |
| `profile` | `"hashline-light"` | Home profile for coordinator runs |
| `job_token_limit` | `100000` | Token reservation cap per job |
| `daily_token_limit` | `500000` | Token reservation cap per UTC day |
| `job_cost_limit_usd_micros` | `5000000` | Cost reservation cap per job |
| `daily_cost_limit_usd_micros` | `20000000` | Cost reservation cap per UTC day |

Enabled configuration requires a positive `app_id`, a nonempty `private_key_file`, a nonempty `model`, an existing home profile, and 1 to 100 allowlisted repositories. `poll_interval_seconds` MUST be 60 to 3600. `concurrency` MUST be 1 to 4. `jobs_per_day` MUST be 1 to 100. `job_token_limit` MUST be 1000 to 1000000. `daily_token_limit` MUST be at least the job cap and at most 10000000. Cost limits MUST be positive, and the daily limit MUST be at least the job limit.

Secret references MUST be exactly one recognized secret-store placeholder. Literal secrets MUST NOT appear in `webhook_secret_ref` or `relay_token_ref`. Relay keys are all-or-none: a partial relay table MUST fail loading.

The `[secrets]` table MUST load in both products. Its key `patterns` MUST be a list of RE2 patterns. Its key `builtin` MUST be a boolean, and it defaults to `true`. The masking contract is in [secrets.md](secrets.md).

```toml
[secrets]
patterns = ["token-[a-z0-9]{8,}"]
builtin = false
```

The interactive settings MUST use the keys in the table below.

| Key | Values | Default | Wire name |
|---|---|---|---|
| `verbosity` | `low`, `medium`, `high` | `medium` | `verbosity` |
| `theme` | `auto`, `auto-soft`, `night`, `soft-night`, `day`, `soft-day` | `auto` | `theme` |
| `icons` | `auto`, `unicode`, `nerd` | `auto` | `icons` |
| `status_hide` | `effort`, `approval`, `sandbox`, `folder`, `queue`, `cost`, `context`, `mark` | `[]` | `statusHide` |
| `notifications` | `true`, `false` | `true` | `notifications` |
| `edit_review.enabled` | `true`, `false` | `false` | `editReview` |

The `auto` theme MUST follow the system and use `night` or `day`. The `auto-soft` theme MUST follow the system and use `soft-night` or `soft-day`. The `status_hide` key MUST NOT name `model`.

`Host.WriteSettings` MUST apply each interactive setting to the running host at once. A rejected edit MUST leave the running settings and `settings.toml` unchanged. A saved `approval` or `sandbox` MUST NOT change a thread that is already running. Each thread keeps the values that its journal records.
Live setting writes MUST apply the same product restrictions as file loading. `dal` MUST reject dalgona-only roots before persistence, including `edit_review.enabled`.

When no layer sets `[approval_decider] threshold`, its value MUST be `0.9`.

The loader MUST validate approval, sandbox, reasoning, tool mode, durations, and runner options. Durations MUST use Go duration syntax. Invalid durations MUST fail loading. A sandbox0 runner MUST use a template and MUST NOT set an image.

The top-level `tls_ca_file` key names a PEM file of certificate authorities. Its default is the empty string. A relative path MUST resolve against the product home. Both products accept the key. `Load` MUST validate the file when the key is set. The file MUST exist, MUST be a regular file, MUST hold at most 1048576 bytes, and MUST hold a PEM certificate. The error MUST report the setting file, its 1-based line, and the resolved path. The message starts with `tls_ca_file` and the resolved path. It ends with `no such file`, `not a regular file`, `larger than 1048576 bytes`, or `holds no PEM certificate`.

A local `login` for an OAuth provider or with `--device-auth` MUST install `tls_ca_file` before its first request. The `models` command sends its request through the host. The host MUST install the setting when it loads its configuration. A `logout` and an API-key `login` send no request and do not read the setting.

A `[remotes.<name>]` table records a box daemon, legacy daemon relay, SSH machine, or paired device.
Its keys are `kind`, `service`, `image`, `template`, `url`, `box_id`, `token_file`, `ssh_destination`, `ssh_platform`, `ssh_install`, `machine_id`, and `public_key_pin`.
The `kind` value MUST be `box`, `daemon`, `ssh`, or `device`, with `box` as the default.
A `box` remote follows Mode B in [runners.md](../../dalgona/spec/runners.md#mode-b-the-daemon-in-a-box).
A `daemon` remote MUST set `url` and `token_file`.
It MUST NOT set `service`, `image`, `template`, or `box_id`.
A `box` remote MUST NOT set `token_file`.
Each key MUST merge across layers on its own.
A key present in a later layer MUST replace the earlier value, including an empty string.
An absent key MUST keep the earlier value.
Other keys inside the table MUST fail loading as unknown keys.

An `ssh` remote MUST set `ssh_destination`, `ssh_platform`, and `machine_id`.
The destination MUST be an OpenSSH host alias or `user@host`.
The platform MUST be `linux`, `darwin`, or `windows`.
The machine identity MUST contain exactly 64 lowercase hexadecimal characters.
An optional `ssh_install` MUST contain the SHA-256 digest that selects the installed artifact.
An empty `ssh_install` MUST select the trusted installed CLI on the remote path.
SSH remotes MUST NOT set box settings, relay settings, or a public-key pin.
Onboarding MUST establish the machine binding before it publishes this configuration.

A `device` remote MUST set `url`, `token_file`, `machine_id`, and `public_key_pin`.
The URL MUST be a canonical HTTPS origin without a path, user information, query, or fragment.
The machine identity and public-key pin MUST each contain exactly 64 lowercase hexadecimal characters.
Device remotes MUST NOT set box or SSH settings.
The device token file MUST contain a JSON-encoded bearer string.
The client MUST validate that file's ownership, type, permissions, size, and content at each dial.
A missing or invalid device token file MUST fail only that endpoint's connection.
It MUST NOT prevent local use or select the legacy daemon transport.

A daemon remote `url` MUST use `https` or `wss` and include a host.
It MUST NOT contain user information, a query, or a fragment.
The client MUST join `/rpc` to the configured URL path.
Both schemes select the same TLS-protected relay transport.

A relative `token_file` path MUST resolve against the product home.
The token file MUST be a regular file and MUST NOT be a symlink.
On non-Windows systems, the token file MUST NOT have group or other permission bits.
A daemon token file MUST contain at most 4096 bytes and a nonempty token after trimming whitespace.
`Load` MUST validate these file rules for a daemon remote.
Device token files MUST retain the same file safety and size limits at dial time.
The endpoint session MUST reread `token_file` at every dial.
Clients MUST use TLS 1.3 as the minimum version and trust the system pool plus `tls_ca_file` when set.
They MUST send the token in the `Authorization` header and MUST NOT expose it through a URL, log, or GUI restore state.

Reported errors MUST append these exact message bodies to the configuration file and line origin:

```text
remotes.<n>.kind must be one of box, daemon, ssh, device
remotes.<n>: a daemon remote takes only kind, url, and token_file
remotes.<n>.url must be an https:// or wss:// URL with a host and no user info, query, or fragment
remotes.<n>.token_file is required for a daemon remote
remotes.<n>.token_file applies only to kind = "daemon" or "device"
```

Reported `token_file` errors MUST use one of the following exact message bodies:

```text
remotes.<n>.token_file <path>: no such file
remotes.<n>.token_file <path>: not a regular file
remotes.<n>.token_file <path>: group or others have access; run chmod 600 <path>
remotes.<n>.token_file <path>: larger than 4096 bytes
remotes.<n>.token_file <path>: holds no token
```

A `[models."<provider>/<model>"]` table sets values for one model. Its keys are `context_window`, `input_usd_per_mtok`, `output_usd_per_mtok`, `cache_read_usd_per_mtok`, and `cache_write_usd_per_mtok`. Each key MUST merge across layers on its own. An absent key MUST keep the earlier value.

The four price keys are TOML numbers in US dollars per million tokens. `Load` MUST round each price to a whole micro-USD per million tokens. A price MUST be between 0 and 10000. The error names the table and the key, as in `[models."<id>"] <key> must be between 0 and 10000`.

`input_usd_per_mtok` and `output_usd_per_mtok` MUST be set together. A cache key without both MUST fail with `[models."<id>"] input_usd_per_mtok and output_usd_per_mtok must be set together`. An absent cache key MUST price those tokens at the input rate. Each price error reports the file and its 1-based line.

Edit styles MUST be one of anchor, replace, hashline, hashline-light, hashline-enhanced, apply_patch, simple, balanced, or strict. A scalar style MUST apply to every model. A style table MUST contain `default`. Model patterns MUST match the full model name without regard to case. `*` matches any run and `?` matches one character. The first row in TOML source order wins.

A custom keyword name MUST match `^[a-z][a-z0-9_-]{1,31}$`. A custom keyword MUST set exactly one of `notice` or `text`. A notice path MUST be relative to the product home and MUST name an existing regular file. A built-in keyword override MAY set only selected fields, including `enabled = false`.

`WriteSettings` MUST update only `settings.toml`. It MUST apply all of its edits, or none. It MUST preserve `config.toml`. It MUST preserve the source order of existing `edit_style` rows. It MUST decode and validate the new document before replacing the file. Invalid keys and values MUST leave the prior file unchanged. It MUST write through a temporary file and rename it into place.

Workspace trust MUST persist as the top-level `mcp_trusted_workspaces` list of workspace paths in `settings.toml`. Only `WriteSettings` and `TrustWorkspace` write this key. An empty list means no workspace is trusted. Every entry MUST be a nonempty workspace path. `TrustWorkspace` MUST store the workspace it adds in canonical form (`filepath.Abs` plus `EvalSymlinks`, falling back to `Clean`). `dal` MUST reject the key as dalgona-only.

```toml
mcp_trusted_workspaces = ["/work/repo"]
```
