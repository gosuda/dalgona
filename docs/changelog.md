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
