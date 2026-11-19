### Workstream: Image input

#### Outcome of the image-input workstream

A client sends an image with a prompt or a steer, through `turn/start` or `turn/steer`, as a `data:` URL or as an absolute local path. The daemon checks the type and size at the boundary. It stores the bytes once as a content-addressed journal blob and journals the image as an attachment of the `user.message` with a masked path. Every adapter that can encode images sends the image in its provider's format. A text-only model refuses the turn with `this model does not accept images`. Recovery rebuilds the request from the blob and never opens the image path again. `model/list` reports `inputModalities` per model, so both clients know which models accept images before a user sends one.

#### Decisions of the image-input workstream

1. Accepted input variants. The server accepts Codex `image` with a `data:` URL in `url`, and `localImage` with an absolute `path`. The vendored definitions are `ImageUserInput` (`type` `image`, `url` or `fileId`) and `LocalImageUserInput` (`type` `localImage`, `path`) in the `UserInput` block at `internal/rpc/testdata/codex-schema/v2/TurnStartParams.json:555-779`. Rival: also accept `fileId` and `http(s)` URLs. Rejected: `fileId` needs a Files API this daemon does not have, and an `http(s)` URL makes the daemon fetch a URL that the client chose. The member `detail` is ignored. No client sends it, and the server skips it (Step 2).

2. Local path trust. The daemon reads any absolute path that its own user can read. Rival: confine reads to the thread `cwd`. Rejected: the terminal workstream, Step 31 names files outside the workspace, such as desktop screenshots. `fs/readFile` in the protocol workstream, Step 17 already reads absolute paths. The path must be absolute, and the server stores `filepath.Clean(path)`. Rival: resolve a relative path against `cwd`. Rejected: the vendored schema defines the path as `AbsolutePathBuf`, which must be absolute.

3. Storage. Each image is one journal blob, named by its SHA-256 and written through `Thread.PutBlob` (`internal/agent/extension.go:75`) and `journal.Store.PutBlob` (`internal/journal/journal.go:573`). An image is always a blob, never inline. Rival: inline base64 in the event. Rejected: `MaxInlineBytes` (`internal/agent/event/event.go:36`) caps inline payloads at 64 KiB, and a small photo exceeds it. Rival: store the path and read the file on each request. Rejected: the file can change, and the journal would not hold the bytes the user sent.

4. Limits and error texts. The limits are 5 MiB (5242880 bytes) per image, 10 images per request, and 10 MiB (10485760 bytes) of image data per request. The Claude API accepts 10 MB of base64 per image, and 5 MiB decodes to 6990508 base64 bytes (vision page, Request limits). The RPC reader accepts one line of at most 16 MiB (`internal/rpc/conn.go:187-188`). Ten MiB decodes to 13981016 base64 bytes, which leaves room for JSON. The terminal workstream, Step 31 and the web workstream, Step 35 use the same 5 MiB per image. Rival: a per-turn limit. Rejected: a turn spans several requests (steers), and the transport limit applies per request. The exact error texts are in Step 2 and Step 1.

5. Type check. The server uses `http.DetectContentType` on the first bytes (standard library). It accepts `image/png`, `image/jpeg` and `image/gif`. Rival: accept WebP, which Anthropic accepts. Rejected: no client converts images, and the terminal and web workstreams accept the same three types. Rival: decode the dimensions with `image.DecodeConfig`. Rejected: provider dimension limits are the provider's rule, and the decode adds CPU and decoder imports. The declared type in a data URL must equal the sniffed type. Rival: trust the declared type. Rejected: a mismatch sends bytes that the model reads as another type.

6. Masking. Image bytes never enter the masker or the keyword scanner. `docs/spec/secrets.md` defines masking for text, and a placeholder inside binary data would corrupt the image. The image path is text, so the path passes `Host.mask` (`internal/agent/tools.go:710`) before journaling. A secret inside pixels is not detected. This is a known gap.

7. Recovery. The turn reads each image from its blob, and each read checks the SHA-256, the byte count and the sniffed type. A failed check fails the turn. Recovery never opens the image path. Rival: reread the path, which `docs/spec/recovery.md` (opening paragraph) allows for workspace files. Rejected for the reason in Decision 3 of this workstream. The rule is `docs/spec/recovery.md`: recovery reads only the journal, the blob store, the runner and the workspace files (`docs/spec/recovery.md:3`), and a turn with no call pending calls the model again (`docs/spec/recovery.md:24`).

8. Which adapters send images. These adapters send images by rule: `anthropic`, `openai` (Responses) and `google`. Three adapters send images only when their metadata says so, per model: `chatgpt` (the catalog `input_modalities`; its Responses encoder sends the image, `internal/model/chatgpt/chatgpt.go:69`), `chatcompat` (`architecture.input_modalities`), and `devin` (`SupportsImages`, `internal/model/devin/wire/discovery.go:152`). The Devin wire field is `ChatMessagePrompt.Images`, `internal/model/devin/wire/chat.go:54`. These adapters refuse: `cursor`, because `internal/model/cursor` has no image field (a search for image, modality and vision finds nothing), and `antigravity`. The antigravity envelope wraps the Gemini request (`internal/model/antigravity/antigravity.go:154`), but the reference `.references/oh-my-pi/packages/ai/src/providers/google-gemini-cli.ts` contains no image part. Rival: send `inlineData` inside the envelope. Rejected in this plan. A live call to the Antigravity envelope with an `inlineData` part would reverse it. This workstream runs no such call. An adapter that does not implement `model.ImageSender` gets the refusal from Step 7, and its encoder never runs.

9. Refusal timing. The check runs in `worker.request` (`internal/agent/request.go:111`) on every model request. Rival: check at submit. Rejected: the model belongs to each step (`w.settings.Model`), and a submit check would build a provider registry to find it. The turn ends `failed` through the existing outcome path (`failure` at `internal/agent/turn.go:181-186`). The image stays journaled, and the user can choose another model and send again.

10. History images for text-only models. These images become the text `[image omitted: this model does not accept images]`. Rival: refuse every request while an image sits in the window. Rejected: a `/model` switch would then block the thread until compaction.

11. Steer. `turn/steer` uses the same parser as `turn/start`. Rival: reject images on steer. Rejected: a steer is a `user.message` of kind `steer` (`docs/spec/events.md`), and a second rule would add an error path only.

12. Journal form. `user.message` gains the optional member `images`. Each entry has `mime`, `path` (masked, optional) and `image` (a blob reference). Rival: a new event type `user.image`. Rejected: one user input would span two events, and the fold and render rules for `user.message` (`docs/spec/events.md`) would need new cases.

13. One content-part type. The model request carries `model.Image` in `Message.Images`, and every adapter encodes that type. Rival: a `Part` interface with text and image variants. Rejected: it rewrites every text path for one more kind. `agent.Image` (input) and `event.Image` (journal) are not content parts. They carry a path and a blob reference.

14. Input modalities. Each adapter sets `model.Model.Images` in `Models`. A listed model of the Anthropic, OpenAI and Google adapters reports `["text","image"]`. A listed model of any other adapter reports it only when the provider metadata says that the model accepts images. The metadata used: Devin `SupportsImages` (`internal/model/devin/wire/discovery.go:152`), OpenRouter `architecture.input_modalities`, and the Codex catalog for the static ChatGPT list (`.references/codex/codex-rs/models-manager/models.json`, `input_modalities` at lines 10, 183, 359, 535, 706, 873, 1014, 1155 and 1522, one per static entry). An unlisted model takes the answer of `SendsImages()`. Rival: a static table in `internal/model/presets.go`. Rejected: presets are role tables, and a table would copy adapter knowledge. The Anthropic, OpenAI and Google list responses have no modality field (`internal/model/anthropic/anthropic.go:413-426`, `internal/model/openai/openai.go:267-299`, `internal/model/google/google.go:456-530`), so their models report images by rule. The live check of those endpoints is a contingency of this workstream.

15. Compaction. The compaction request carries text only. `internal/agent/compaction.go:193-195` builds it from `renderSpan` (`compaction.go:231`), which renders text. The images in the compacted span survive only as summary text. Rival: send images to the summarizer. Rejected: it multiplies cost and the request limits.

16. Echo on resume. The `userMessage` item echoes each image as `{"type":"image","url":"dalgona-blob:<sha>","mime":...,"bytes":...,"path":...}`, without bytes. Rival: echo a data URL. Rejected: a resumed thread would send several megabytes of base64 per image.

17. Format sources. Each adapter encodes the format named in its Step, and each Step cites the page it reads.

#### Image-input step 1. Image limits and type detection

Files: `internal/agent/event/event.go` (constants next to `MaxInlineBytes` at line 36), `internal/agent/image.go` (new), `internal/agent/image_test.go` (new).

Symbols:

In `internal/agent/event/event.go:36`, next to `MaxInlineBytes`, add these constants:

```go
const MaxImages = 10
const MaxImageBytes = 5 << 20
```

In `internal/agent/image.go`, add this code:

```go
const MaxRequestImageBytes = 10 << 20
type Image struct { Path, Mime string; Data []byte }
func ImageMIME(data []byte) (string, error)
func CheckImages(images []Image) error
```

Behaviour: `ImageMIME` calls `http.DetectContentType(data)`. It returns `image/png`, `image/jpeg` or `image/gif`. Any other result gives the error `unsupported image type <type>; send PNG, JPEG or GIF`. Empty data gives `text/plain; charset=utf-8` and the same error. `CheckImages` returns, in this order: `a request takes at most 10 images, got <n>` when there are more than `MaxImages`; `image <i> is <n> bytes; the limit is 5242880 bytes (5 MiB)` for the first image above `MaxImageBytes` (1-based index); and `images total <n> bytes; the limit is 10485760 bytes (10 MiB) per request` when the sum is above `MaxRequestImageBytes`. `CheckImages` does not check the type. The constants live in package `event`. `CheckImages` in package `agent` uses them as `event.MaxImages` and `event.MaxImageBytes`. No alias exists. The type is checked in Steps 2 and 4.

Error handling: this step has no I/O. Missing and unreadable files are handled in Step 2. Wrong type and too large are returned by `ImageMIME` and `CheckImages` with the texts above.

Failing-first tests:
- `TestImageMIME` in `internal/agent/image_test.go`. Inputs: `"\x89PNG\r\n\x1a\n"` gives `image/png`. `"\xff\xd8\xff\xe0"` gives `image/jpeg`. `"GIF89a"` gives `image/gif`. `"RIFF\x00\x00\x00\x00WEBPVP8 "` gives the error `unsupported image type image/webp; send PNG, JPEG or GIF`. Expected values come from the sniffing table of `net/http` (`DetectContentType`).
- `TestCheckImages` in `internal/agent/image_test.go`. Inputs: 11 one-byte images give `a request takes at most 10 images, got 11`. One image of 5242881 bytes gives `image 1 is 5242881 bytes; the limit is 5242880 bytes (5 MiB)`. Three images of 4194304 bytes give `images total 12582912 bytes; the limit is 10485760 bytes (10 MiB) per request`. Expected values come from the arithmetic in the test and the limits in Decision 4 of this workstream.

Specification: `docs/spec/protocol.md`, new subsection "Input images". Sentences:
- The server MUST accept at most 10 images in one request.
- The server MUST accept an image of at most 5242880 bytes.
- The server MUST accept at most 10485760 bytes of image data in one request.
- The server MUST accept only PNG, JPEG and GIF image data.
- The server MUST detect the image type from the first bytes of the data.

- Anchor: Tony Hoare, Claude Shannon

#### Image-input step 2. Input variants on `turn/start` and `turn/steer`

Files: `internal/rpc/protocol.go:85-90` (`InputItem`), `internal/rpc/serve_turn.go:14-23` (`turnParams`), `:81-99` (`prompt`), `:110-125` (`readInput`), `:181-` (`turnSteer`), `internal/rpc/serve_image.go` (new), `internal/rpc/serve_image_test.go` (new).

Symbols:

In `internal/rpc/protocol.go:85-90`, the `InputItem` struct gains this member:

```go
URL string `json:"url,omitzero"`
```

In `internal/rpc/serve_turn.go:14-23`, the `turnParams` struct gains this member:

```go
images []agent.Image
```

In `internal/rpc/serve_image.go`, a new file, add these functions:

```go
func imageFromURL(raw string) (agent.Image, error)
func imageFromPath(path string) (agent.Image, error)
func (p *turnParams) checkedImages() ([]agent.Image, error)
```

`readInput` gets `url` in its `stringMembers` map and two cases. For `image`, it calls `imageFromURL(url)`. For `localImage`, it calls `imageFromPath(path)`. It appends the result to `p.images`. The `detail` member is skipped. `prompt` and `turnSteer` both call `checkedImages`, which runs `agent.CheckImages` and wraps its error with `invalidParams`. `prompt` sets `Input.Images`. `turnSteer` sets `Input.Images` too.

`imageFromURL` behaviour, in order:
1. It requires the prefix `data:` and a `,` separator, and a header that ends with `;base64`. Otherwise it returns `image url must be a data: URL with base64 data`.
2. It sets `declared` to the header without `;base64`.
3. It checks the payload length before it decodes. A payload longer than `base64.StdEncoding.EncodedLen(event.MaxImageBytes)` gives the Step 1 text for one image, `image 1 is <n> bytes; the limit is 5242880 bytes (5 MiB)`, where `<n>` is `base64.StdEncoding.DecodedLen(len(payload))`, the largest size the payload can decode to.
4. `base64.StdEncoding.DecodeString` failure gives `image data is not valid base64`.
5. `agent.ImageMIME(data)` gives the type. A type that differs from `declared` gives `image type <type> does not match the data URL type <declared>`.

`imageFromPath` behaviour, in order:
1. A path that is not absolute gives `image path must be absolute: <path>`.
2. A path longer than 4096 bytes gives `image path is longer than 4096 bytes`.
3. `os.Stat` runs first. A path that is not a regular file (a FIFO, a device or a directory) gives `image path must be a regular file: <path>` with `-32602`, because `os.Open` blocks on a FIFO. Then `os.Open` with `fs.ErrNotExist` gives `image file not found: <path>`. Any other open error gives `internalError` with `cannot read image <path>: <cause>`.
4. `f.Stat()` gives the size. A size above `event.MaxImageBytes` gives the Step 1 text for one image, `image 1 is <n> bytes; the limit is 5242880 bytes (5 MiB)`, where `<n>` is that size. A failed `Stat` gives `internalError` with `cannot read image <path>: <cause>`.
5. `io.ReadAll(io.LimitReader(f, event.MaxImageBytes+1))` reads the file. A read error gives `internalError` with `cannot read image <path>: <cause>`. A directory fails the regular-file check. A read of more than `event.MaxImageBytes` bytes, because the file grew, gives the same Step 1 text with `<n>` the bytes read.
6. `agent.ImageMIME` gives the type. Its error is returned with `invalidParams`.

Wire literals (Codex input items): `{"type":"image","url":"data:image/png;base64,<base64>"}` and `{"type":"localImage","path":"/abs/shot.png"}`. An `image` item without a `data:` URL gives `-32602`. A missing file gives `-32602`. A read failure gives `-32603`. A wrong type gives `-32602`. Too large gives `-32602`.

Failing-first tests:
- `TestTurnStartLocalImagePath` in `internal/rpc/serve_image_test.go`. Input: a file `t.TempDir()/shot.png` with the bytes `"\x89PNG\r\n\x1a\n"` plus `"data"` (12 bytes). The request is `turn/start` with input `[{"type":"text","text":"what is this"},{"type":"localImage","path":"<that path>"}]`. Expected: the journal `user.message` holds one image with `mime` `image/png` and `image.bytes` 12. The expected value comes from the byte count written by the test.
- `TestTurnStartImageDataURL` in the same file. Input: `{"type":"image","url":"data:image/png;base64,iVBORw0KGgo="}`. Expected: `image.bytes` 8. The expected value comes from decoding the literal, which is the PNG signature.
- `TestTurnImageErrors` in the same file, a table. Expected code and message for each row:
  - a relative path `shot.png`: `-32602`, `image path must be absolute: shot.png`.
  - a missing absolute path: `-32602`, `image file not found: <path>`.
  - a directory path: `-32602`, text `image path must be a regular file: <path>`.
  - an `https://` URL: `-32602`, `image url must be a data: URL with base64 data`.
  - a WebP header, which is `RIFF`, four zero bytes and `WEBP`, as base64: `-32602`, `unsupported image type image/webp; send PNG, JPEG or GIF`.
  - a PNG payload declared as `image/jpeg`: `-32602`, `image type image/png does not match the data URL type image/jpeg`.
  - a `localImage` file of 5242881 bytes: `-32602`, `image 1 is 5242881 bytes; the limit is 5242880 bytes (5 MiB)`.
  - a payload of `!!!`: `-32602`, `image data is not valid base64`.
  - a path of 4097 bytes: `-32602`, `image path is longer than 4096 bytes`.
  The expected values come from the messages in this step.

- `TestTurnSteerAcceptsImage` in `internal/rpc/serve_image_test.go`. Input: a running turn, then `turn/steer` with the input `[{"type":"localImage","path":"<that path>"}]`, where the file holds the 12 bytes of the first failing-first test. Expected: the journal `user.message` of kind `steer` holds one image with `image.bytes` 12. The expected value comes from the byte count written by the test.

Specification: `docs/spec/protocol.md`, the `turn/start` and `turn/steer` rows. Sentences:
- The server MUST accept an `image` input item only with a `data:` URL.
- The server MUST accept a `localImage` input item only with an absolute path.
- The server MUST ignore the `detail` member of an image input item.
- The server MUST read a local image file that the daemon user can read.
- The server MUST return `-32602` for an image that is missing or has the wrong type.

- Lands after: Steps 1, 3 and 4 (the limits, the journal form and the blob store). The protocol workstream, Step 9 lands first, because it edits `serve_turn.go`, where `prompt` lives. Cite these functions by name, not by line.

- Anchor: David Parnas, Tony Hoare

#### Image-input step 3. Journal form of an image attachment

Files: `internal/agent/event/event.go` (`Image` type after `Mention` at lines 56-60; `UserMessage` at 185-198; `Validate` at 201-221), `internal/agent/event/encode.go` (`userMessageMembers` at 93-112, new `imageValue` beside `mentionValue` at 119-122), `internal/agent/event/decode.go` (`decodeUserMessage` at 32-51, new `decodeImage` beside `decodeNotice` at 53-62), `internal/agent/event/codec_test.go` (tests).

Symbols:

In `internal/agent/event/event.go`, add this type after `Mention` at lines 56-60:

```go
type Image struct {
	Mime string
	Path string
	Blob BlobRef
}
```

In `internal/agent/event/event.go`, the `UserMessage` struct at lines 185-198 gains this field:

```go
Images []Image
```

In `internal/agent/event/event.go`, the `Validate` method at lines 201-221 calls this function:

```go
validateImages(e.Images)
```

In `internal/agent/event/event.go`, add this function:

```go
func validateImages(images []Image) error
```

Behaviour: in package `event`, `validateImages` returns `a message takes at most 10 images` when there are more than `MaxImages`. For each image it checks, in order, that `Mime` is `image/png`, `image/jpeg` or `image/gif` (error `images[%d]: mime must be image/png, image/jpeg or image/gif`), that `Blob.Bytes` is between 1 and `MaxImageBytes` (error `images[%d]: bytes must be between 1 and 5242880`), and that `validSHA("image", Blob.Blob)` passes (error `images[%d]: image must be a lowercase SHA-256 digest`). `Path` is not validated here, because it is masked before the journal (Step 4). The encoder writes `path` only when `Path` is not empty. The image is always a blob reference, and no inline form exists. The encoder writes `image` as an object, as `optionalBlobField` writes `text_blob` (`internal/agent/event/encode.go:314`). `decodeUserMessage` reads `images` with `r.array`, and `decodeImage` reads `mime`, `path` and `image` (with `r.blob("image")`).

Wire and journal literal, inside `ev`:

```json
{"type":"user.message","item_id":"item-01","turn_id":"turn-01","text":"what is this","kind":"prompt","images":[{"mime":"image/png","path":"/home/u/shot.png","image":{"blob":"<64 lowercase hex>","bytes":12}}]}
```

Error handling: an image that fails validation fails `Append` before it writes (`docs/spec/journal.md`, Append). No other error path exists in this step. Missing and unreadable files are handled in Step 2.

Failing-first tests, all in `internal/agent/event/codec_test.go`:
- `TestUserMessageImagesRoundTrip`. Input: a `UserMessage` with one image `{Mime:"image/png", Path:"/work/shot.png", Blob:{Blob: strings.Repeat("a", 64), Bytes: 12}}`. Expected: encode then decode returns the same struct. The expected value is the input.
- `TestUserMessageImageJournalForm`. Input: the same image. Expected: the encoded `ev` equals the literal JSON above, with `<64 lowercase hex>` replaced by 64 `a` characters. The expected value comes from the literal in this step.
- `TestUserMessageImageValidate`, a table. Mime `text/plain` gives `images[0]: mime must be image/png, image/jpeg or image/gif`. Bytes 0 gives `images[0]: bytes must be between 1 and 5242880`. Eleven images give `a message takes at most 10 images`. A blob name `xyz` gives `images[0]: image must be a lowercase SHA-256 digest`. The expected values come from the messages in this step.

Specification: `docs/spec/events.md`, the `user.message` row, and a new paragraph "Image attachments". This step adds only that row and that paragraph; the edit-review workstream, Step 2 adds the judged rows. Sentences:
- A `user.message` MUST carry at most 10 images in its `images` member.
- Each image MUST name a blob of PNG, JPEG or GIF data.
- An image MUST NOT use an inline payload.
- An image path MUST be masked before the event is journaled.
- A `verbatim` message carries its images like any other message.

- Lands after: Step 1 (the constants).

- Anchor: David Parnas, Claude Shannon

#### Image-input step 4. Submit stores the image blob and masks the path

Files: `internal/agent/types.go:105-114` (`Input` gains `Images []Image`), `internal/agent/entry.go:14-24` (`preparedInput` gains `images []event.Image`), `internal/agent/entry.go:55` (`prepareInput`), `internal/agent/entry.go:122-126` (the `event.UserMessage` built in `actor.submit`, which gains `Images: in.images`), `internal/agent/image.go` (`prepareImages`), `internal/agent/image_test.go`.

Symbols:

In `internal/agent/image.go`, add this method:

```go
func (t *Thread) prepareImages(images []Image) ([]event.Image, error)
```

Behaviour: `prepareImages` calls `CheckImages`, then `ImageMIME` on each image, and fails with `image type <type> does not match its declared type <Mime>` when the sniffed type differs from `Mime`. For each image it calls `t.host.mask(img.Path)` when the path is not empty (`internal/agent/tools.go:710`). Then it calls `t.PutBlob(img.Data)` (`internal/agent/extension.go:75`) and appends `event.Image{Mime: img.Mime, Path: masked, Blob: blob}`. `prepareInput` calls `prepareImages` after it masks the text, and before the `verbatim` return. The result goes into `preparedInput.images`, and `actor.submit` copies it into the message. A masking failure returns the `MaskError` and fails the submit (`docs/spec/secrets.md`). A blob write failure returns `agent: store blob: <cause>` (`extension.go:78`). A blob written before a rejected submit stays in `blobs/` unreferenced. This is harmless, because blobs are content-addressed.

Failing-first test:
- `TestSubmitImageStoresBlobAndMasksPath` in `internal/agent/image_test.go`. Setup: `t.Setenv("TEST_API_KEY", "sk-test-0123456789")`, then build the harness as `reviewHarness` does (`internal/agent/keyword_mask_test.go:33`, which calls `auth.NewWithPatterns`). The environment source masks every variable whose name ends in `_API_KEY` (`docs/spec/secrets.md`). Input: `Input{Kind: InputPrompt, Text: "what is this", Images: []Image{{Path: "/tmp/keys/sk-test-0123456789/shot.png", Mime: "image/png", Data: []byte("\x89PNG\r\n\x1a\nxyz")}}}`. Expected: the journaled `images[0].path` equals `/tmp/keys/{{secret:TEST_API_KEY}}/shot.png`. The expected `images[0].image.blob` equals the lowercase hex SHA-256 of the input data, computed in the test. The expected `images[0].image.bytes` equals the input length (11). The expected source for the placeholder grammar is `docs/spec/secrets.md` (`{{secret:<ENV_NAME>}}`).

Specification: `docs/spec/secrets.md`, after the paragraph on read snapshots. Sentences:
- Image bytes MUST NOT pass through the masker.
- An image path MUST be masked before it enters the journal.
- A failed path mask MUST fail the submit that carries the image.

`docs/spec/journal.md`, section "Blobs". Sentence:
- An image MUST be stored as a blob, whatever its size.

- Lands after: Steps 1 and 3 (`CheckImages` and `event.Image`).

- Anchor: John Carmack

#### Image-input step 5. Model content part, transcript and blob integrity

Files: `internal/model/model.go:70-76` (`Message` gains `Images []Image`), `internal/model/model.go:77-81` (new type after `ToolResult`), `internal/agent/context.go:17-31` (`transcript`: `deferred` and `followups` change to `[]userTurn`), `:132-139` (`user`), `:163-169` (`note`), `:171-177` (`emitUser`), `:179-185` (`emitFollowups`), `:232-237` (the deferred flush), `internal/agent/image.go` (`imageParts`), `internal/agent/image_test.go`.

Symbols:

In `internal/model/model.go`, add this type after `ToolResult` at lines 77-81:

```go
type Image struct { Mime string; Data []byte }
```

The `Message` struct in `internal/model/model.go:70-76` gains the field `Images []Image`.

In `internal/agent/context.go`, add this type:

```go
type userTurn struct { text string; images []model.Image }
```

In `internal/agent/image.go`, add this method:

```go
func (h *Host) imageParts(images []event.Image) ([]model.Image, error)
```

Behaviour: `user` builds a `userTurn` from `renderUser` and from `b.host.imageParts(e.Images)`, because `transcript` holds `host *Host` (`internal/agent/context.go:18`). A load error goes to `b.keep`, which fails `messages()`. `emitUser` takes a `userTurn` and builds `model.Message{Role: "user", Text: turn.text, Images: turn.images}`. `note` wraps its text in a `userTurn` with no images. This step moves every caller of `emitUser` to `userTurn`: `user`, `note`, `emitFollowups` and the deferred flush at `context.go:236`. The edit-review workstream, Step 5 passes its feedback text as a `userTurn` with no images. `imageParts` does, for each image, unless the transcript already verified that blob name (a map keyed by blob name, filled on the first read): `h.journal.Blob(img.Blob.Blob)` (`internal/journal/journal.go:616`), then `sha256.Sum256`, then `hex.EncodeToString`, which must equal the name, then the length, which must equal `Blob.Bytes`, then `ImageMIME`, which must equal `Mime`. The journal read does not check the content hash (`journal.go:616-630`), so this check is the blob integrity check.

Error texts: a missing blob gives `agent: load image blob <sha>: <cause>`. A content, length or type mismatch gives `agent: image blob <sha> does not match its journal record`.

Recovery: `recoverTurn` (`internal/agent/recover.go:33`) ends with `drive` (`internal/agent/turn.go:112`), which calls the model through `loop`, `callModel` and `request` as any turn does. The request is built by `messages()`, so the image comes from the blob, and recovery never opens the path.

Failing-first tests, both in `internal/agent/image_test.go`:
- `TestRecoveredTurnSendsImageFromBlob`. Input: a journal with a `turn.started` and a `user.message` that holds one PNG image (`"\x89PNG\r\n\x1a\nabc"`) with path `/nonexistent/shot.png`. The turn has no assistant message. The host recovers, and a fake provider records the request. Expected: `Messages[0].Images[0].Data` equals the 11 input bytes, and recovery returns no error. The expected value comes from the input bytes, and the missing path proves that recovery does not open it.
- `TestImageBlobMismatchFailsTurn`. Input: the same journal, after the test overwrites `blobs/<sha>` with `"\x89PNG\r\n\x1a\nzzz"`. Expected: `turn.completed` with `status` `failed` and `error` `agent: image blob <sha> does not match its journal record`, where `<sha>` is the SHA-256 of the original bytes computed in the test.

Specification: `docs/spec/recovery.md`, a new section "Image input". This step adds only that section; the edit-review workstream, Step 4 owns the recovery table row. Sentences:
- Recovery MUST read each image of a turn from its blob.
- Recovery MUST NOT open the path of an image.
- A blob MUST match its SHA-256, its byte count and its image type before the server sends it.
- A failed blob check MUST fail the turn with the error text of the spec.

`docs/spec/events.md`, section "Model context". This step adds only this sentence; the edit-review workstream, Step 5 adds its own sentence to the section. Sentence:
- An image of a `user.message` MUST render in the same model message as the text of that message.

- Lands after: Steps 1, 3 and 4 (`ImageMIME`, `event.Image` and the blob store).

- Anchor: David Parnas, Tony Hoare

#### Image-input step 6. `inputModalities` in `model/list`

Files: `internal/model/model.go:98-104` (`Model` gains `Images bool`), `internal/model/model.go` (new `ImageSender` interface beside `Provider`), `internal/rpc/types.go:106-115` (the rpc `Model` gains `InputModalities`), `internal/rpc/serve_misc.go:65-68` (`modelList` fills it), `internal/agent/request.go:261-279` (`capabilities`), `internal/agent/image.go` (`sendsImages`), `internal/rpc/serve_models_test.go` (new), `internal/model/devin/devin.go:349-371` (`listedModel` sets `Images: config.SupportsImages`), `internal/model/devin/devin_test.go`.

Symbols:

In `internal/model/model.go`, add this interface beside `Provider`:

```go
type ImageSender interface{ SendsImages() bool }
```

The `Model` struct in `internal/model/model.go:98-104` gains the field `Images bool`.

In `internal/rpc/types.go:106-115`, the rpc `Model` struct gains this field:

```go
InputModalities []string `json:"inputModalities"`
```

In `internal/rpc/serve_misc.go`, add this function:

```go
func inputModalities(images bool) []string
```

In `internal/agent/image.go`, add this function:

```go
func sendsImages(registry *model.Registry, provider string) bool
```

Behaviour: `inputModalities(true)` returns `["text","image"]`, and `inputModalities(false)` returns `["text"]`. The member is always written. `capabilities` sets a local `listed` to true when a listing entry matches. When `listed` is false, it sets `reported.Images` to `sendsImages(registry, provider)`, which is true when the registered adapter implements `ImageSender` and returns true. A listed model keeps the value from its listing. The Devin adapter's `listedModel` sets `Images` from `config.SupportsImages`. The Devin adapter has no `SendsImages` method, so its unlisted models refuse images. The vendored schema says `inputModalities` has a default of `["text","image"]` (`internal/rpc/testdata/codex-schema/v2/ModelListResponse.json:94-103`). The server writes the member in every response, so the default never applies.

Error handling: this step has no input. The fake provider of the test returns models without I/O.

Failing-first test:
- `TestModelListCarriesInputModalities` in `internal/rpc/serve_models_test.go`. Input: a fake provider whose `Models` returns `[{ID:"text-only"}, {ID:"vision", Images:true}]`. Expected: `text-only` has `inputModalities` `["text"]`, `vision` has `["text","image"]`, and the response validates against `ModelListResponse.json`. The expected values come from the rule in Decision 14 of this workstream. The validation uses the wire client's `checkResponse` (`internal/rpc/serve_harness_test.go:455-458`), which already checks `model/list` against `v2/ModelListResponse.json` (line 294).
- `TestListedModelCarriesSupportsImages` in `internal/model/devin/devin_test.go`. Input: a `ClientModelConfig` with `SupportsImages` true, and another with it false. Expected: `Images` true and false. The expected values come from the field.

Specification: `docs/spec/protocol.md`, the `model/list` row. This step adds only the `inputModalities` sentences; the protocol workstream, Step 11 keeps the other members of that row. Sentences:
- Each model in `model/list` MUST report `inputModalities`.
- The server MUST report `["text","image"]` for a model that accepts images.
- The server MUST report `["text"]` for every other model.

`docs/spec/providers.md`, the paragraph of `## Capabilities and roles` that begins `Models` MUST use the provider list endpoint. Sentences:
- The Anthropic, OpenAI and Google adapters MUST report images for each listed model.
- Every other adapter MUST report images for a model only when the provider metadata says that the model accepts images.

- Lands after: Step 5 (both edit `internal/model/model.go`). The protocol workstream, Step 11 (`modelList`) lands first, in phase 7.

- Anchor: Herbert Simon

#### Image-input step 7. Refusal for a model without image input

Files: `internal/agent/request.go:111-121` (`worker.request`), `internal/agent/image.go` (`turnHasImages`, `withoutImages`), `internal/agent/image_test.go`.

Symbols:

```go
func turnHasImages(v view, turnID string) bool
func withoutImages(messages []model.Message) []model.Message
```

Behaviour: `worker.request` calls `capabilities` as it does now. If `caps.Images` is false and `turnHasImages(v, w.turnID)` is true, it returns `errors.New("this model does not accept images")`. This error ends the turn as `failed` through `runTurn` (`runTurn` at `internal/agent/turn.go:93-99` starts the loop, and `failure` at `:181-186` ends the turn). If `caps.Images` is false and the current turn has no image, `worker.request` replaces `messages` with `withoutImages(messages)`. That function returns a copy, and it appends `[image omitted: this model does not accept images]` to the text of each message that has images. Each image adds one notice, each preceded by `\n\n`. A message without images is unchanged. `turnHasImages` reads `v.records` as `turnPrompt` does (`request.go:137`), and it checks each `event.UserMessage` with the turn id and `len(Images) > 0`.

Error handling: the refusal is the error of this step. The turn keeps its journaled image, and the user can send again with another model. The clients cite this step for the text and declare no copy of it.

Failing-first tests, both in `internal/agent/image_test.go`:
- `TestTextOnlyModelRefusesCurrentTurnImage`. Input: a fake provider with no `SendsImages` method. The client submits a prompt with one image. Expected: `turn.completed` with `status` `failed` and `error` `this model does not accept images`. The expected value comes from the text of this step.
- `TestTextOnlyModelOmitsHistoryImage`. Input: turn 1 with an image sent to a vision fake model, then turn 2 with text only sent to a text-only fake model. Expected: the request of turn 2 has no `Images` on any message, and its text contains `[image omitted: this model does not accept images]`.

Specification: `docs/spec/providers.md`, a new paragraph "Image input". Sentences:
- A provider that does not implement image input MUST refuse a turn that carries an image.
- The server MUST refuse with `this model does not accept images`.
- The server MUST replace the images of earlier turns with a text notice for such a model.

- Lands after: Steps 5 and 6 (`Message.Images`, `Model.Images` and `caps.Images`). The protocol workstream, Step 3 lands first, because it edits `worker.request`. Cite that function by name.

- Anchor: Edsger W. Dijkstra

#### Image-input step 8. Anthropic sends image blocks

Files: `internal/model/anthropic/anthropic.go:184-205` (`encodeMessage`, user branch: the gate at line 192 also passes when `len(Images) > 0`, and the images precede the text block), `:249-255` (`encodeTextBlock`; a new `encodeImageBlock` follows it), `:423` (the `model.Model` literal in `Models`, which starts at `:413`, gains `Images: true`), new method `func (p *Provider) SendsImages() bool { return true }`, `internal/model/anthropic/anthropic_test.go`.

Symbols:

```go
func encodeImageBlock(encoder *model.JSONEncoder, image model.Image)
```

Wire literal, one content block per image, before the text block:

```json
{"type":"image","source":{"type":"base64","media_type":"image/png","data":"iVBORw0KGgo="}}
```

Behaviour: the `data` value is `base64.StdEncoding.EncodeToString(image.Data)`. The text block keeps its `cache_control` marker when it has no tool results. Images go first, because the vision guide recommends image before text (Tip, "Send images to Claude"). Errors: none in this step. Input errors are handled in Steps 2 to 5.

Failing-first test:
- `TestEncodeRequestSendsImageBlock` in `internal/model/anthropic/anthropic_test.go`. Input: a `model.Message{Role:"user", Text:"What is this?", Images:[{Mime:"image/png", Data:"\x89PNG\r\n\x1a\n"}]}`. Expected: the user message content is `[{"type":"image","source":{"type":"base64","media_type":"image/png","data":"iVBORw0KGgo="}},{"type":"text","text":"What is this?"}]`. The expected base64 is the PNG signature encoded with the standard alphabet. The source is the "Base64-encoded image example" in the vision page.

Specification: `docs/spec/providers.md`, the Anthropic paragraph of `## Requests` (the one that begins `Anthropic MUST use`). Add this sentence after its existing sentences, and keep the effort sentence that the core-fixes workstream, Step 3 wrote there. Sentence:
- The Anthropic adapter MUST send each image as a base64 `image` block before the text block of its message.

- Lands after: Steps 5, 6 and 7 (the content part, `Model.Images` and the refusal). The core-fixes workstream, Steps 3 and 4 land first, because they edit `anthropic.go`.

- Anchor: Richard Feynman

#### Image-input step 9. OpenAI Responses and ChatGPT send `input_image`

Files: `internal/model/openai/openai.go:159-164` (`encodeMessage`), `:201-212` (`encodeTextMessage` gains `images []model.Image`; the images precede the `input_text` object, and the text object is written when the text is not empty or the message has no images), `:296` (the `model.Model` literal in `Models`, which starts at `:267`, gains `Images: true`), new `func (p *Provider) SendsImages() bool { return true }`, `internal/model/openai/openai_test.go`. `internal/model/chatgpt/chatgpt.go:34-38` (`type staticModel` gains the field `images bool`), `:48-56` (each of the nine entries sets `images: true`, because each static entry lists `image` in the catalog), `:108-115` (the loop in `Models` sets `Images: item.images` in its `model.Model` literal), `internal/model/chatgpt/chatgpt_test.go`.

Wire literal, a user input item:

```json
{"role":"user","content":[{"type":"input_image","image_url":"data:image/png;base64,iVBORw0KGgo=","detail":"auto"},{"type":"input_text","text":"What is this?"}]}
```

Behaviour: `detail` is always `auto`, because Decision 1 of this workstream ignores the client value. The message gate at line 162 also passes when `len(Images) > 0`. A message with no images keeps its current bytes. Errors: none in this step.

Failing-first tests:
- `TestEncodeRequestSendsInputImage` in `internal/model/openai/openai_test.go`. Input: a user message with text `What is this?` and one PNG image with the signature bytes. Expected: the item equals the wire literal above. The source is the Responses example in the images guide (`input_image`, data URL, `detail: "auto"`).
- `TestStaticModelsAcceptImages` in `internal/model/chatgpt/chatgpt_test.go`. Input: `Models` of the ChatGPT provider. Expected: every entry has `Images` true. Add a second case. It sets `staticModels` to one entry whose `images` is false, calls `Models`, and expects that model's `Images` to be false. It restores `staticModels` with `t.Cleanup`. The source is `input_modalities` with `image` on each entry in `.references/codex/codex-rs/models-manager/models.json`, whose nine static entries each list `image` (`input_modalities` at lines 10, 183, 359, 535, 706, 873, 1014, 1155 and 1522).

Specification: `docs/spec/providers.md`, the OpenAI paragraph of `## Requests` (the one that begins `The OpenAI provider MUST use`) and the ChatGPT paragraph of `## Subscription and IDE providers` (the one that begins ``The `chatgpt` provider``). Sentences:
- The OpenAI adapter MUST send each image as an `input_image` item with a data URL.
- The OpenAI adapter MUST send `detail` as `auto`.
- The ChatGPT adapter MUST send images through the OpenAI Responses encoder.

- Lands after: Steps 5, 6 and 7.

- Anchor: Claude Shannon, Richard Feynman

#### Image-input step 10. Google sends `inlineData`

Files: `internal/model/google/google.go:169-185` (`encodeContent`: the user branch at `:177` writes `parts` with the images first, then the text part; its gate is `message.Text != "" || len(message.Images) > 0`, and the text part is written only when the text is not empty), `:210-217` (`encodeTextPart` is unchanged), new `encodeImagePart(encoder *model.JSONEncoder, image model.Image)`, the `model.Model` literal at `:526` in `Models` (`:456`) gains `Images: true`, new `func (p *Provider) SendsImages() bool { return true }`, `internal/model/google/google_test.go`. `internal/model/antigravity` does not change. It has no `SendsImages` method, so Step 7 refuses before the envelope is built.

Wire literal, one part per image:

```json
{"role":"user","parts":[{"inlineData":{"mimeType":"image/png","data":"iVBORw0KGgo="}},{"text":"What is this?"}]}
```

Behaviour: `inlineData` and `mimeType` use the names of the Gemini `Blob` type (`aistudio.google.com/api-reference/generate-content`, `Part.inlineData: Blob`). The `data` member is base64 text. The repository's Gemini client writes base64 text in the same field (`.references/gemini-cli/packages/core/src/utils/pathReader.test.ts:117-121`, `fileUtils.ts:743-747`). Errors: none in this step.

Failing-first test:
- `TestEncodeContentsSendsInlineData` in `internal/model/google/google_test.go`. Input: a user message with text `What is this?` and one PNG image with the signature bytes. Expected: the `contents` entry equals the wire literal above. The source is the `pathReader.test.ts` expectation for `inlineData`, with `data` set to base64.

Specification: `docs/spec/providers.md`, the Google paragraph of `## Requests` (the one that begins `Google MUST use`). Sentences:
- The Google adapter MUST send each image as an `inlineData` part with a `mimeType` and base64 `data`.
- The Google adapter MUST place image parts before the text part of its message.

- Lands after: Steps 5, 6 and 7 (the content part, `Model.Images` and the refusal, which keeps Antigravity out of this encoder).

- Anchor: Claude Shannon

#### Image-input step 11. Chat-compatible adapters send `image_url` and read OpenRouter modalities

Files: `internal/model/chatcompat/chatcompat.go:152-157` (`encodeTextMessage`, unchanged), `:175-189` (`encodeAssistantOrUser`: the user content is an array when `len(Images) > 0`, with the images first and the text part last), `:277-286` (`modelList.Data` gains `Architecture struct { InputModalities []string json:"input_modalities" } json:"architecture"`), `:288-303` (`decodeModels` sets `Images` from `acceptsImages(item.Architecture.InputModalities)`), new `func acceptsImages(modalities []string) bool { return slices.Contains(modalities, "image") }`; the chat-compatible adapter has no `SendsImages` method, so an unlisted model refuses images, `internal/model/chatcompat/chatcompat_test.go`.

Wire literal:

```json
{"role":"user","content":[{"type":"image_url","image_url":{"url":"data:image/png;base64,iVBORw0KGgo="}},{"type":"text","text":"What is this?"}]}
```

Behaviour: a user message without images keeps the current string `content`. The `architecture.input_modalities` member comes from OpenRouter's model list (`openrouter.ai/docs/api/api-reference/models/list-all-models-and-their-properties`, `ModelArchitecture`). A model that lists modalities without `image` reports `Images` false. A model without the field reports false. Image input is proven only by the provider's own modality metadata. Errors: none in this step.

Failing-first tests, both in `internal/model/chatcompat/chatcompat_test.go`:
- `TestEncodeRequestSendsImageURL`. Input: a user message with text `What is this?` and one PNG image with the signature bytes. Expected: the message equals the wire literal above. The source is the Create chat completion reference (`ChatCompletionContentPartImage`: `image_url.url`, `type` `image_url`, and the text part `type` `text`).
- `TestDecodeModelsReadsInputModalities`. Input: `{"data":[{"id":"a/text-only","architecture":{"input_modalities":["text"]}},{"id":"a/vision","architecture":{"input_modalities":["text","image"]}},{"id":"b/plain"}]}`. Expected `Images`: false, true and false. The expected values follow the rule in Decision 14 of this workstream and the OpenRouter schema.

Specification: `docs/spec/providers.md`, two paragraphs. Add the `image_url` sentence to the paragraph of `## Requests` that begins `Chat-compatible provider`. Add the list sentence to the paragraph of `## Capabilities and roles` that Step 6 also edits, after Step 6's sentences. Sentences:
- The chat-compatible adapter MUST send each image as an `image_url` part with a data URL.
- A chat-compatible model MUST report images only when its list says that its input modalities include `image`.

- Lands after: Steps 5, 6 and 7.

- Anchor: Edward Tufte

#### Image-input step 12. Devin sends `Images` on the prompt and reads `SupportsImages`

Files: `internal/model/devin/request.go:173-196` (`promptBuilder.add`: the user prompt gains `Images: devinImages(message.Images)`; the condition at line 190 already covers a message without tool results), new `func devinImages(images []model.Image) []wire.ImageData`, `internal/model/devin/devin_test.go`. The `SupportsImages` mapping is Step 6's.

Symbols:

In `internal/model/devin/request.go`, add this function:

```go
func devinImages(images []model.Image) []wire.ImageData
```

Each element of the result has this form:

```go
wire.ImageData{Base64DataView: []byte(base64), MimeTypeView: []byte(mime)}
```

Wire fields: `ChatMessagePrompt.Images` is field 10 (`internal/model/devin/wire/chat.go:53-54`, `:80-81`). `ImageData` has `Base64DataView` at field 1, `MimeTypeView` at field 2 and `CaptionView` at field 3 (`internal/model/devin/wire/nested.go:327-336`). The caption stays empty. Errors: none in this step.

Failing-first tests, both in `internal/model/devin/devin_test.go`:
- `TestBuildPromptsCarriesImages`. Input: a user message with one PNG image with the signature bytes. Expected: the prompt has `Images[0].MimeTypeView` equal to `image/png` and `Images[0].Base64DataView` equal to `iVBORw0KGgo=`. The expected values come from encoding the input.

Specification: `docs/spec/providers.md`, the Devin chat paragraph (the one that begins `Devin chat MUST send`). Sentences:
- The Devin adapter MUST send each image in the `Images` field of its user prompt.

- Lands after: Steps 5, 6 and 7 (`Message.Images`, `Model.Images` and the refusal).

- Anchor: Richard Feynman

#### Image-input step 13. The `userMessage` item echoes image metadata

Files: `internal/rpc/serve_track.go:136-139` (`userMessage` gives the item its images), `internal/rpc/serve_items.go:37-44` (the user content writer adds one `image` object per image after the text), the `Item` struct in `internal/rpc/types.go:53` (gains `Images []ItemImage`), `internal/rpc/serve_image_test.go`.

Symbols:

The new type `ItemImage` is:

```go
type ItemImage struct { Type string `json:"type"`; URL string `json:"url"`; Mime string `json:"mime"`; Bytes int64 `json:"bytes"`; Path string `json:"path,omitzero"` }
```

The `Item` struct in `internal/rpc/types.go:53` gains the field `Images []ItemImage`.

Wire literal, after the text item in `content`:

```json
{"type":"image","url":"dalgona-blob:<64 lowercase hex>","mime":"image/png","bytes":12,"path":"/home/u/shot.png"}
```

Behaviour: `userMessage` sets `Type` to `image`, `URL` to `dalgona-blob:` followed by the blob name, `Mime` and `Bytes` from the journal record, and `Path` to the masked path. `Path` is omitted when empty. The `url` names the blob and carries no bytes. The writer in `serve_items.go` encodes each `ItemImage` with those tags. `ImageUserInput` allows extra members (`TurnStartParams.json`, `ImageUserInput` has no `additionalProperties` limit), so the item validates against the vendored schema. Errors: none in this step. Missing and unreadable files are handled in Step 2.

Failing-first test:
- `TestUserMessageItemEchoesImage` in `internal/rpc/serve_image_test.go`. Input: a turn with the text `what is this` and one PNG image of 12 bytes from Step 2. Expected: the user item `content` is `[{"type":"text","text":"what is this"}, {"type":"image","url":"dalgona-blob:<sha>","mime":"image/png","bytes":12,"path":"<path>"}]`, where `<sha>` is the SHA-256 of the input bytes computed in the test.

Specification: `docs/spec/protocol.md`, the `user.message` row in the notifications table. This step adds only its three sentences to that row; the protocol workstream, Steps 2 and 7 own the rest of it. Sentences:
- The server MUST echo each image of a user message as an `image` input item.
- The echoed image item MUST NOT carry image bytes.
- The echoed image item MUST name the blob with the `dalgona-blob:` prefix.

- Lands after: Steps 2, 3 and 4 (the image record and its blob). The protocol workstream, Steps 2 and 7 land first, because they edit the notifications table and `userMessage`.

- Anchor: Dieter Rams

#### Schemas of the image-input workstream

Input variants on `turn/start` and `turn/steer` (wire, camelCase on the Codex side):

| Input item | Members | Rule |
|---|---|---|
| `text` | `text` | existing |
| `image` | `url` (a `data:<mime>;base64,<data>` URL), `detail` (ignored) | MIME is `image/png`, `image/jpeg` or `image/gif`, and equals the sniffed type |
| `localImage` | `path` (absolute, at most 4096 bytes), `detail` (ignored) | file of at most 5242880 bytes that the daemon user can read |

The Codex names and fields are in `internal/rpc/testdata/codex-schema/v2/TurnStartParams.json`: `UserInput` (line 555), `ImageUserInput`, `LocalImageUserInput`, and `ImageDetail` with values `auto`, `low`, `high` and `original` (line 269).

Errors on `turn/start` and `turn/steer`:

| Code | Message |
|---|---|
| `-32602` | `image path must be absolute: <path>` |
| `-32602` | `image path is longer than 4096 bytes` |
| `-32602` | `image file not found: <path>` |
| `-32603` | `cannot read image <path>: <cause>` |
| `-32602` | `image url must be a data: URL with base64 data` |
| `-32602` | `image data is not valid base64` |
| `-32602` | `unsupported image type <type>; send PNG, JPEG or GIF` |
| `-32602` | `image type <type> does not match the data URL type <declared>` |
| `-32602` | `a request takes at most 10 images, got <n>` |
| `-32602` | `images total <n> bytes; the limit is 10485760 bytes (10 MiB) per request` |
| `-32602` | `image <i> is <n> bytes; the limit is 5242880 bytes (5 MiB)` |
| `failed` turn | `this model does not accept images` (Step 7) |

Journal (`ev` of a `user.message`), new member `images`:

```json
{"type":"user.message","item_id":"item-01","turn_id":"turn-01","text":"what is this","kind":"prompt","images":[{"mime":"image/png","path":"{{secret:TEST_API_KEY}}/shot.png","image":{"blob":"<64 lowercase hex>","bytes":12}}]}
```

Blob file: `blobs/<64 lowercase hex>`, which holds the exact image bytes.

Model list (`model/list` result), new member:

```json
{"id":"openai/gpt-6-astra","model":"openai/gpt-6-astra","inputModalities":["text","image"]}
```

Provider request fragments:

Anthropic, `POST /messages`, `content` of a user message (vision page, "Base64-encoded image example"):

```json
{"role":"user","content":[{"type":"image","source":{"type":"base64","media_type":"image/png","data":"iVBORw0KGgo="}},{"type":"text","text":"What is this?"}]}
```

OpenAI and ChatGPT, `POST /responses`, `input` of a user message (`developers.openai.com/api/docs/guides/images-vision`, Responses examples):

```json
{"role":"user","content":[{"type":"input_image","image_url":"data:image/png;base64,iVBORw0KGgo=","detail":"auto"},{"type":"input_text","text":"What is this?"}]}
```

Google, `POST /models/{model}:generateContent`, `contents` entry (`aistudio.google.com/api-reference/generate-content`, `Part.inlineData: Blob`; the base64 `data` form is from `.references/gemini-cli/packages/core/src/utils/pathReader.test.ts:117-121`):

```json
{"role":"user","parts":[{"inlineData":{"mimeType":"image/png","data":"iVBORw0KGgo="}},{"text":"What is this?"}]}
```

Chat-compatible, `POST /chat/completions`, `messages` entry (`developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create`, `ChatCompletionContentPartImage`):

```json
{"role":"user","content":[{"type":"image_url","image_url":{"url":"data:image/png;base64,iVBORw0KGgo="}},{"type":"text","text":"What is this?"}]}
```

Devin, `ChatMessagePrompt` (wire, `internal/model/devin/wire/chat.go:53-54`, `nested.go:327-336`). The repository's wire type is the only source for this format. Its acceptance by the live server is a contingency of this workstream:

```
ChatMessagePrompt{Source: user, PromptView: "What is this?", Images: [{Base64DataView: "iVBORw0KGgo=", MimeTypeView: "image/png"}]}
```

OpenRouter model list, read for input modalities (`openrouter.ai/docs/api/api-reference/models/list-all-models-and-their-properties`): `architecture.input_modalities` is an array of `text` and `image`.

#### Checks of the image-input workstream

Run after each step, before its commit: `just verify` (the `verify` recipe at `justfile:121`). Then run the step's named tests:

1. `go test -count=1 -run 'TestImageMIME|TestCheckImages' ./internal/agent`
2. `go test -count=1 -run 'TestTurnStartLocalImagePath|TestTurnStartImageDataURL|TestTurnImageErrors|TestTurnSteerAcceptsImage' ./internal/rpc`
3. `go test -count=1 -run 'TestUserMessageImages|TestUserMessageImageJournalForm|TestUserMessageImageValidate' ./internal/agent/event`
4. `go test -count=1 -run 'TestSubmitImageStoresBlobAndMasksPath' ./internal/agent`
5. `go test -count=1 -run 'TestRecoveredTurnSendsImageFromBlob|TestImageBlobMismatchFailsTurn' ./internal/agent`
6. `go test -count=1 -run 'TestModelListCarriesInputModalities' ./internal/rpc` and `go test -count=1 -run 'TestListedModelCarriesSupportsImages' ./internal/model/devin`
7. `go test -count=1 -run 'TestTextOnlyModel' ./internal/agent`
8. `go test -count=1 -run 'TestEncodeRequestSendsImageBlock' ./internal/model/anthropic`
9. `go test -count=1 -run 'TestEncodeRequestSendsInputImage' ./internal/model/openai` and `go test -count=1 -run 'TestStaticModelsAcceptImages' ./internal/model/chatgpt`
10. `go test -count=1 -run 'TestEncodeContentsSendsInlineData' ./internal/model/google`
11. `go test -count=1 -run 'TestEncodeRequestSendsImageURL|TestDecodeModelsReadsInputModalities' ./internal/model/chatcompat`
12. `go test -count=1 -run 'TestBuildPromptsCarriesImages' ./internal/model/devin`
13. `go test -count=1 -run 'TestUserMessageItemEchoesImage' ./internal/rpc`

After Step 13, run `go test -race -count=1 ./internal/agent/... ./internal/rpc/... ./internal/model/...`.

Manual check. Run it after phase 15, when the terminal workstream, Step 31 and the web workstream, Step 35 have landed. It is a smoke run of this workstream alone, before the live sessions of phase 21, and it is not one of the scenarios T01 to T48. It uses the live-debug workstream, Step 1 (`internal/llmstub`) and Step 3 (the recipes):
1. Run `just live-up` (live-debug workstream, Step 3). This starts the scripted chat-completions server with `POST /v1/chat/completions` and `GET /v1/models`. Its models carry `architecture.input_modalities` `["text","image"]` only after the live-debug workstream, Step 1 adds that member to the `GET /v1/models` response. That step does not add it yet, so this check stays open until it does. Without it the stub's models refuse images.
2. Point a chat-compatible provider at the stub, as the live-debug workstream describes.
3. Create the image: `printf '\211PNG\r\n\032\nxxxx' > /tmp/shot.png`. The file has 12 bytes.
4. Send `turn/start` with the input `[{"type":"text","text":"describe the image"},{"type":"localImage","path":"/tmp/shot.png"}]`. Use the terminal paste of the terminal workstream, Step 31 or the web composer of the web workstream, Step 35.
5. Read the stub recording (`{"seq":<n>,"path":"/v1/chat/completions","turn":"<id>","body":{...}}`). Expected: the body of the image turn contains `"type":"image_url"` and the URL `data:image/png;base64,` followed by the output of `base64 -w0 /tmp/shot.png`. The expected value comes from that command.
6. Run `just live-down`.

#### Prerequisites of the image-input workstream

Protocol workstream (`protocol`):
- The protocol workstream, Step 11 (`model/list` presets, phase 7) lands before Step 6 here, because both edit `modelList` at `internal/rpc/serve_misc.go:53-73`. Protocol Step 11 MUST NOT add the `inputModalities` member, the `Model.Images` field or the Devin mapping. Step 6 here adds them, with its test `TestListedModelCarriesSupportsImages`.
- The protocol workstream, Step 1 is not needed here. The Step 6 test uses the wire client's existing `checkResponse`, which already checks `model/list` against `v2/ModelListResponse.json`.
- The protocol workstream, Step 9 lands before Step 2 here. Steps 2 and 7 of the protocol workstream land before Step 13 here. Step 3 of the protocol workstream lands before Step 7 here.
- The core-fixes workstream, Steps 3 and 4 land before Step 8 here.
- The edit-review workstream, Step 5 passes its feedback text to `emitUser` as a `userTurn`, as Step 5 here requires. The edit-review workstream, Step 4 owns the recovery table row.
- `InputItem.URL` (`internal/rpc/protocol.go:85-90`) is owned by Step 2 here. The terminal workstream cites this step for the image input item.
- This workstream adds no `dalgona/` message, so no `dalgona-schema` file is needed.

Client workstreams that use this workstream:
- The terminal workstream, Step 31 (image paste) MUST land after Steps 2 and 6 here. Its 5 MiB limit and its three types match Steps 1 and 2.
- The web workstream, Step 35 (paste chips and image paste) MUST land after Steps 2 and 6 here. Its 15 MiB relay check is below the 16 MiB line limit (`internal/rpc/conn.go:187-188`).
- The terminal workstream, Step 21 and the web workstream, Step 29 (model panels) show `inputModalities` after Step 6 here.

Live-debug workstream:
- The manual check uses the live-debug workstream, Step 1 (`internal/llmstub`) and Step 3. That stub's `decodeRequest` reads only text parts of `content`, so an image part does not stop a scripted turn from matching. Its `GET /v1/models` does not list `architecture.input_modalities` yet. The live-debug workstream, Step 1 MUST add that member before the manual check can pass. This is an open item.

Internal order (phase 8): 1, 3, 4, 2, 5, 6, 7, 8, 9, 10, 11, 12, 13. Each step's Lands-after line names its prerequisites. Steps 8 to 12 need Steps 5 to 7, and Step 13 needs Steps 2 to 4.

