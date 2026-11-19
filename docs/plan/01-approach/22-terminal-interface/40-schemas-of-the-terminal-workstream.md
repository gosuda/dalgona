#### Schemas of the terminal workstream

This workstream adds no journal event, tool argument or extension method of its own. It reads six environment variables through `EnvMsg.Getenv` (`bubbletea/v2@v2.1.1/environ.go:24`) and consumes the interfaces below.

Environment:

| Name | Type | Rule | Example |
|---|---|---|---|
| `NO_COLOR` | string | any non-empty value turns colour off; bold, underline and reverse stay | `NO_COLOR=1` |
| `DAL_NO_MOTION` | string | any non-empty value stops the breathing dot's animation and the cursor blink, for both products. The dot stays on its first frame. The one-second tick chain still runs for the elapsed time and the rate-limit countdown | `DAL_NO_MOTION=1` |
| `KITTY_WINDOW_ID` | string | any non-empty value sends the one font query of step 12 at start, when `icons` is `auto` | `KITTY_WINDOW_ID=1` |
| `TERM` | string | `xterm-kitty` selects the `ansi.DesktopNotification` sequence (step 36) | `TERM=xterm-kitty` |
| `TERM_PROGRAM` | string | `iTerm.app`, `WezTerm` and `ghostty` select the `ansi.Notify` sequence; other values fall back to BEL | `TERM_PROGRAM=WezTerm` |
| `TMUX` | string | any non-empty value makes the notification a BEL | `TMUX=/tmp/tmux-1000/default,1,0` |

Config keys (snake_case, top level of `settings.toml`, written through `config/value/write` or `config/batchWrite`, read through `config/read` `config`; added by protocol workstream item P7):

| Key | Type | Values | Default | Example |
|---|---|---|---|---|
| `theme` | string | `auto`, `auto-soft`, `day`, `soft-day`, `night`, `soft-night`; `auto` and `auto-soft` follow the system | `auto` | `theme = "night"` |
| `verbosity` | string | `low`, `medium`, `high` | `medium` | `verbosity = "high"` |
| `icons` | string | `auto`, `unicode`, `nerd`; the browser ignores it | `auto` | `icons = "nerd"` |
| `status_hide` | array of strings | item ids of the Design record, Status band table: `effort`, `approval`, `sandbox`, `folder`, `queue`, `context`, `mark`; `model` is rejected with `status_hide: model cannot be hidden`. Wire field `statusHide` | `[]` | `status_hide = ["folder", "mark"]` |
| `notifications` | boolean | `true`, `false`. When `false`, the terminal sends no desktop notification (step 36). Wire field `notifications`. Owner: protocol Step 8 (`Config.Notifications`) and Step 10 (`settingKey`) | `true` | `notifications = false` |

Fixed fields. Each row has these widths and places. Time-driven values keep their cells.

| Field | Cells | Place | Rule |
|---|---|---|---|
| state word | 8 | live row, left | left-aligned, padded or cut to 8 cells with `fit` |
| elapsed time | 7 | live row, after the state word | right-aligned in 7 cells (`formatElapsed`, Step 11) |
| live dot | 1 | live tag | `glyph.live.on` or `glyph.live.off` |
| `effort`, `approval`, `sandbox` | 1 each | status band, left group | one glyph |
| `folder` | 1, one space, then free text | status band, left group | glyph (U+25B8 Unicode, U+F0256 Nerd), one space, folder name through `fit` |
| `queue` | 4 (Unicode), 6 (Nerd) | status band, left group | `+` and the count right-aligned in 3 cells; Nerd adds the glyph and one space before the field |
| `context` | 4 (Unicode), 6 (Nerd) | status band, right group | percent right-aligned in 4 cells; Nerd adds the moon glyph and one space |
| `mark` | free text | status band, right group | product name through `fit` |

Wire messages consumed (camelCase; owners in Dependencies):

- P1 `fuzzyFileSearch` request `{query: string, roots: string[]}`; response `{files: [{root: string, path: string, file_name: string, score: uint32, match_type: "file", indices?: uint32[]}]}` (`internal/rpc/testdata/codex-schema/FuzzyFileSearchParams.json`, `FuzzyFileSearchResponse.json`). Example: `{"query":"modl","roots":["/repo"]}`.
- P2 `command/exec` request `{command: string[] (non-empty), cwd?: string, timeoutMs?: int64, outputBytesCap?: uint}`; response `{exitCode: int32, stdout: string, stderr: string}` (`codex-schema/v2/CommandExecParams.json`, `CommandExecResponse.json`). Example: `{"command":["sh","-c","git status"],"cwd":"/repo","timeoutMs":120000,"outputBytesCap":65536}`.
- P3 `item/fileChange/patchUpdated` carries `changes` before the request (protocol Schemas), with `additions` and `deletions`. Shape: `changes: [{path: string, kind: "add"|"update"|"delete", diff: string, additions: int, deletions: int, diffOmitted?: bool}]`. `diffOmitted` is a bool, default false and omitted when false; the server sets it when either side is over 1 MiB (cross-phase decision 42), and then `diff` is empty. The same field is added to `FileChange` (`internal/rpc/types.go:67-70`). A change over the limit reads `{"path":"big.bin","kind":"update","diff":"","diffOmitted":true}`. Example: `{"threadId":"t1","turnId":"u1","itemId":"c1","startedAtMs":1,"reason":"patch a.go","changes":[{"path":"a.go","kind":"update","diff":"-a\n+b"}]}`.
- P5 `thread/tokenUsage/updated` `tokenUsage.modelContextWindow: int` set when known (`internal/rpc/types.go:218-226`).
- P6 `model/list` items gain `presets: string[]` with values `default`, `smol`, `slow` (protocol Step 11). Example item: `{"id":"anthropic/claude-haiku-5-5","presets":["smol"],"supportedReasoningEfforts":[{"reasoningEffort":"low","description":"low"}]}`.
- P7 `config/read` `config.theme`, `config.verbosity` (enums above); `config/value/write` and `config/batchWrite` accept keyPaths `approval`, `verbosity`, `theme`, `icons`, `status_hide`, `edit_review.enabled` and `notifications`. `config/batchWrite` request `{edits: [{keyPath: string, value: any, mergeStrategy: "replace"}]}`; response `{filePath: string, status: string, version: string}` (`codex-schema/v2/ConfigBatchWriteParams.json`). Example: `{"edits":[{"keyPath":"theme","value":"night","mergeStrategy":"replace"}]}`.
- `dalgona/retry/wait` (the core-fixes workstream, Step 1a): `{threadId: string, turnId: string, remainingMs: int64}`, sent once a second during an attended wait; 0 ends it. Example: `{"threadId":"t1","turnId":"u1","remainingMs":125000}`. The cross-phase decision 41 line reads it.
- P8 `turn/start` input item: its wire form is owned by the image-input workstream, Step 2, and its limits by Step 1 (5 MiB an image, 10 images a request, 10 MiB a request). Step 31 uses those limits.
- L1 `dalgona/account/providers/list` request `{}`; response `{providers: [{id: string, name: string, methods: ("apiKey"|"browser"|"device")[], signedIn: bool}]}` (protocol Step 12). Example: `{"providers":[{"id":"anthropic","name":"Anthropic","methods":["apiKey"],"signedIn":false},{"id":"chatgpt","name":"ChatGPT","methods":["browser","device"],"signedIn":true}]}`.
- N1 `account/login/completed` notification `{success: bool, error?: string, provider: string}` (protocol Step 12; `provider` is new there). Example: `{"success":false,"error":"openai did not accept the API key; it was not saved","provider":"openai"}`.
- Design record, Colour server request `item/tool/requestUserInput`: its params, its answer response and its cancel shape `{answers: {}}` are owned by the ask workstream, Step 3 (cross-phase decision 9). This workstream reads them through `rpc.ToolRequestUserInputParams` and `rpc.ToolRequestUserInputResponse`.

T1. Terminal colour values. The tests-and-docs workstream, Step 19 (`DESIGN.md`) holds every value: the four themes, the seven text roles of Step 9 (`text`, `dim`, `faint`, `accent-text`, `failed-text`, `added-text`, `warning-text`) and the thread hue table indexed by `threadHue`. `DESIGN.md` is the only written source of these values, and no step here copies them. Step 9 loads them into `palettes` and `hues`.

T2. Status icons. The tests-and-docs workstream, Step 19 (`DESIGN.md`) holds the icon values: the Nerd Font codepoints (Nerd Fonts 3.5.1), the Unicode glyphs and their cell rules. No step here copies them. Each Unicode glyph must have East Asian Width `N`, `Na` or `H` (cross-phase decision 18), and the test `TestBandGlyphsHaveNarrowWidth` (Step 12) checks it.

Effort glyphs follow the rank rule of Design record, Icons and step 12. The Unicode effort glyphs are Braille characters, which have width class Neutral.

Glyph tokens (each one cell wide, East Asian Width `N`, `Na` or `H`): `glyph.live.on` is U+25AA and `glyph.live.off` is U+25AB (cross-phase decision 51). The status icons are the T2 values in `DESIGN.md`, and the product mark is the Design record, Status band text, so no other glyph token exists.

