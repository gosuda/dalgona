# Changes by version

This page lists the user-visible changes in each version. The `/changelog` command prints it.

## Unreleased

- A reopened session shows the model, thinking level, approval mode, execution mode, and name its journal recorded. Before, it showed the defaults of the host that opened it. A session that never chose a model still follows the configured default and names no model. Changing another setting no longer makes the default look selected. See [sessions](sessions.md).
- API model routes can name their provider. A route that does, such as a saved default or a row picked in the model picker, resolves exactly that provider. It never resolves another provider or a synthetic model that lists the same id. It fails with a typed error when the provider is unavailable. Routes without a provider resolve as before. A saved default writes `provider` in `[model_route]`. An earlier dalgon cannot load a file that has `provider`. It refuses to start until you remove the line.
- `models/list` rows add `route` and `reference`. The remote model picker selects by route for the session only and scopes by provider. Servers without them keep the older selection by name. The protocol version is still 1.
- Fix remote API-key sign-in outcome reporting. See [remote sign-in](commands.md#remote-sign-in).
- Persist turn identities before acknowledging queued work. Cancelling a queued turn no longer invalidates later journal entries. Restarting a session cannot reuse its reserved identity. Queued text is not restored.
- New turn-ID reservations use journal format 2. Other records retain format 1. Existing journals need no conversion. A reader must support format 2 after a reservation is written. See [durable turn identities](sessions.md#durable-turn-identities).
- Rust code that uses `dal-tools` can call `patch::styles::tier` again, also in constant expressions. It returns the prompt-intensity tier of an edit style.
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
- Fix terminal cancellation reporting. The terminal now says what happened to a cancellation:
  - A refused cancellation request reports its own error while that turn is still running.
  - A transport error while waiting reports that error. The run no longer finishes as if nothing failed.
  - When the turn already finished, quit and resume complete normally with no cancellation line.
  - A lost replay, a closed subscription, or no confirmation in time reports that the cancellation could not be confirmed. It asks you to resume the session to check its state.
  - Resuming after such a failure closes both the old session and the opened replacement session.
  - A turn that ends cancelled still shows the inline cancellation line. Any other end shows no line. Fullscreen never shows the inline line.
  - On quit, an earlier terminal error still wins over a later failure to show the cancellation line.

## 0.1.0 (2026-09-26; no tag, build from source)

- First v0: the headless loop, the terminal, Starlark plugins, the wire surfaces, and this manual. No tagged release exists. See [Install](install.md).

## Where to go next

- [Slash commands](commands.md): run `/changelog` to print this page.
- [Install dal and dalgona](install.md): build the current source.
