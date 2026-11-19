#### Specifications step 26. Correct `docs/spec/recovery.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/spec/recovery.md` in one commit. Line numbers are from commit `3c4b6f94`.

Use one word for one thing.

- Keep `thread`. Replace session at lines 3, 7, 8, 9, 10, 28, 39.
- Keep `subagent`. Replace child at lines 39.

Rewrite these long sentences as written here.

| Line | Write |
|---|---|
| 16 | It MUST NOT stop recovery. The exception is a snapshot whose masked bytes contain a secret placeholder that cannot be unmasked. That snapshot MUST abort the open with an error. That snapshot MUST NOT populate the snapshot store with placeholder text. |
| 50 | The reasoning effort of a recovered turn follows this order. Precedence runs in this order: a keyword with effort `max`, then the `effort` field, then the thread effort, then `reasoning` in config. |
| 52 | A turn step MUST rebuild the tool set when its model differs from the model of the current tool set. The rebuilt tool set makes the edit style follow the model. |

Then make every sentence of the file follow rule 3 of this plan. Split each sentence that holds a semicolon outside code into two sentences. Keep every literal. The file has 3 style findings at commit `3c4b6f94`.

Proof: `uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec/recovery.md` prints `style findings: 0`, and `just verify` passes.

