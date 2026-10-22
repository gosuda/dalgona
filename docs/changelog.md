# Changes by version

## Unreleased

- Fix remote API-key sign-in outcome reporting. See [remote sign-in](commands.md#remote-sign-in).
- Persist turn identities before acknowledging queued work. Cancelling a queued turn no longer invalidates later journal entries, and restarting a session cannot reuse its reserved identity. Queued text is not restored.
- New turn-ID reservations use journal format 2; other records retain format 1. Existing journals need no conversion. A reader must support format 2 after a reservation is written. See [durable turn identities](sessions.md#durable-turn-identities).
- Restore the public `patch::styles::tier` function and its constant-evaluation support.
- Reject unsupported Unicode duration units with a configuration error instead of panicking.
- Report incomplete patch rollback with the affected paths and retained backup locations. Continue restoring other files when one restoration fails.
- Retain service cancellation receipts until service admission consumes them, so repeated cancellations do not strand `ask` or `run` capacity.
- Add `ArtifactFile::StatusTxt` (`status.txt`) to task isolation directories, saved atomically with file mode 0600 on Unix. Rust callers with exhaustive matches on `ArtifactFile` must handle the new variant.
- Azure OpenAI entries resolve their endpoint from an explicit `base_url` first, then `AZURE_OPENAI_BASE_URL`, then `AZURE_OPENAI_RESOURCE_NAME`. A resource name an entry never needs is never validated and cannot fail setup; entry `api_version` and `deployments` values win over the matching environment values. See [Settings in dal.toml](config.md#azure-openai).
- Devin request failures now carry a short readable detail from the server reply when it holds one. Markup pages are skipped, unsafe characters are rejected, the text is bounded, and credentials stay redacted.
- Google account-validation failures now show the verification link the server names, when its error calls for account validation. The link is rendered, never opened on its own; ordinary quota messages are unchanged.
- Add canonical `github-copilot` device sign-in (`dalgon login github-copilot`), supporting optional GitHub Enterprise domains, user verification code display, and completion polling. Stored credentials persist to `auth.json` as an API-key token or structured envelope containing the token, enterprise domain, and discovered account endpoint. Sign-in applies best-effort model policy enablement without fetching the model catalogue at sign-in time. API keys can also be supplied via `COPILOT_GITHUB_TOKEN` or `--api-key`.
- Bedrock provider readiness checks captured AWS credential sources (environment variables, shared configuration and credential files, SSO, credential processes, or instance roles) and stored bearer tokens across effective configured provider families and aliases. `dalgon login status` reports Bedrock entries as `ready` or `not configured` instead of attempting an interactive sign-in flow.
- In GitHub Copilot device sign-in, submitting a blank enterprise domain defaults to `github.com`. Prompts for required OAuth authorization codes or redirect URLs continue to require non-empty input.
- Cancelling CLI device-code authorization with Ctrl+C during completion polling restores the terminal cleanly and cancels the sign-in request without saving incomplete credentials.
- Prefer device authorization over an API key in terminal sign-in when the provider does not offer browser sign-in, matching the interactive CLI.

## 0.1.0 (2026-09-26)

- First v0 release: the headless loop, the terminal, Starlark plugins, the wire surfaces, and this manual.
