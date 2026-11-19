#### Web step 35. Paste chips and image paste

- Composer paste handler.
  - Image files: read as a data URL. Accept png, jpeg and gif within the limits of the terminal workstream, Step 31. An image over 5 MiB shows `Image is larger than 5 MiB.`; an eleventh shows `At most 10 images per message.`; a model whose `inputModalities` from `model/list` lacks `image` gets `this model does not accept images`. Otherwise add a chip `Image · ${name} · ${size}` with `Remove`.
  - Text at the paste-chip threshold of the terminal workstream, Step 28: prevent the insert and add a chip `Pasted text · ${n} lines` with `Show` (an inline read-only `<pre>`) and `Remove`.
- New component. `web/src/lib/components/AttachmentChips.svelte`.
- Send. `turn/start` input is `[{ type: "text", text: [typed, ...pastes].join("\n\n"), text_elements: [] }, ...images.map((i) => ({ type: "image", url: i.dataUrl }))]`.
  - Before sending, if `new Blob([JSON.stringify(params)]).size > 15 * 1024 * 1024`, show `The message and images are too large to send together. Remove an image.` and keep the draft.
- Test. `Composer.dom.test.ts`:
  - `a long paste becomes a chip and is sent after the typed text`
  - `a pasted image is sent as an image input`
  - `an oversized message is kept with an error`
  - Fails before: paste is inserted raw.
- Spec (`docs/spec/web.md`). `### Composer`: chips, limits, and the 15 MiB client check of this step.
- Lands after: Step 25 (the composer keys and the `Composer` component).

Anchor: Italo Calvino.

