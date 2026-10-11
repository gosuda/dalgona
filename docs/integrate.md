# Build a client on the dal protocol

Choose a way in. GUI client, step by step: spawn `dalgon rpc`, send `initialize`, call `session/list`, `session/open`, `session/subscribe`, render updates, answer requests exactly once, cancel when needed, read blobs with `blob/read`, reconnect with `gen` and `seq`. Generate types from `protocol/schema`. Editors connect through ACP.

Rules for clients:

- R1: answer each request exactly once.
- R2: never invent missing updates; resync on gaps.
- R3: cancel cleanly; do not abandon turns.
- R4: keep tokens secret; never log them.
- R5: one adapter per transport; no adapter stacks on another.
- R6: validate server replies tolerantly; ignore unknown fields.
