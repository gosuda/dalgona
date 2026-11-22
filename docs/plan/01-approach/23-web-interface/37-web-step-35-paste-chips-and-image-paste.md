#### Web step 35. Paste chips and image paste

- Composer paste handler.
  - Image files: read as a data URL. Accept png, jpeg and gif within the limits of the terminal workstream, Step 31. An image over 5 MiB shows `Image is larger than 5 MiB.`; an eleventh shows `At most 10 images per message.`; a model whose `inputModalities` from `model/list` lacks `image` gets `this model does not accept images`. Otherwise add a chip `Image · ${name} · ${size}` with `Remove`.
  - Text at the paste-chip threshold of the terminal workstream, Step 28: prevent the insert and add a chip `Pasted text · ${n} lines` with `Show` (an inline read-only `<pre>`) and `Remove`.
- Convert CRLF and lone CR to LF once before line counting. Preserve final LF, validate the entire edit, and keep chip identities outside Unicode text.
- Reject a multiline paste into a single-line field without first-line truncation. Keep the original field value and recoverable candidate.
- New component. `web/src/lib/components/AttachmentChips.svelte`.
- Send the typed text, each accepted paste as a separate text input item, and then image items, following the [web composer contract](../../../dalgona/spec/web.md#composer). Preserve each accepted source sequence. The daemon owns its explicit inter-item assembly.
  - Before sending, count UTF-8 bytes of the complete serialized input array, including escapes and expanded chips. It must fit 15 MiB. The enclosing request must also fit the transport frame. Rejection preserves the draft, selection, and chips.
- Test. `Composer.dom.test.ts`:
  - `a long paste becomes a chip and is sent after the typed text`
  - `a pasted image is sent as an image input`
  - `an oversized message is kept with an error`
  - Fails before: paste is inserted raw.
- Spec (`docs/spec/web.md`). `### Composer`: chips, limits, and the 15 MiB client check of this step.
- Lands after: Step 25 (the composer keys and the `Composer` component).

Anchor: Italo Calvino.

