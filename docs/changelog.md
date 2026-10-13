# Changes by version

## Unreleased

- Fix remote API-key sign-in outcome reporting. See [remote sign-in](commands.md#remote-sign-in).
- Persist turn identities before acknowledging queued work. Cancelling a queued turn no longer invalidates later journal entries, and restarting a session cannot reuse its reserved identity. Queued text is not restored.
- New turn-ID reservations use journal format 2; other records retain format 1. Existing journals need no conversion. A reader must support format 2 after a reservation is written. See [durable turn identities](sessions.md#durable-turn-identities).
- Restore the public `patch::styles::tier` function and its constant-evaluation support.
- Reject unsupported Unicode duration units with a configuration error instead of panicking.
- Report incomplete patch rollback with the affected paths and retained backup locations. Continue restoring other files when one restoration fails.

## 0.1.0 (2026-09-26)

- First v0 release: the headless loop, the terminal, Starlark plugins, the wire surfaces, and this manual.
