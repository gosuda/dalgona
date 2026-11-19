#### Specifications step 10. Correct `docs/spec/edit-styles.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/spec/edit-styles.md` in one commit. Line numbers are from commit `3c4b6f94`.

Replace these statements. The code is right and the text is stale.

| Line | Now | Write | Evidence |
|---|---|---|---|
| 44 | `PUT N`, `PUT N-M`, and `PUT N.=M` replace source lines. `PUT <N`, `PUT >N`, and `PUT >$` insert at source gaps. | `PUT N:`, `PUT N-M:`, and `PUT N.=M:` replace source lines. `PUT <N:`, `PUT >N:`, and `PUT >$:` insert at source gaps. | internal/tool/patch/hashline.go:284-285 `if !ok {` then `return locator{}, 0, expected(line, "PUT with a trailing colon", rest)`; internal/tool/patch/description.go:11 `PUT N: or PUT N-M: replaces original lines`; internal/tool/patch/conformance_extra_test.go: |
| 60 | An update MAY include `*** Move to`. | An update MAY include `*** Move to: <path>`. | internal/tool/patch/apply_patch.go:94 `if raw, ok := strings.CutPrefix(p.lines[cursor].text, "*** Move to: "); ok {` |
| 82 | A placeholder with no known value MUST stay as written. | A placeholder with no known value MUST fail the call. The error MUST name the placeholder. | internal/tool/patch/patch.go:47-48 `if unmaskErr := unmaskEdits(edits, env.Masker.UnmaskStrict); unmaskErr != nil {` then `return tool.Plan{}, fmt.Errorf("patch: secret placeholder cannot be restored: %w", unmaskErr)`; internal/auth/mask.go:106 `return "", fmt |
| 86 | Other runners MUST return `no_replace_unavailable` for those operations. | Other runners MUST return an error whose text contains `no_replace_unavailable` for those operations. | internal/tool/patch/writer.go:149 `return errors.New("patch: no_replace_unavailable on this runner")`; internal/tool/patch/writer.go:303 `return errors.New("no_replace_unavailable")` |

Add these statements. The code already behaves this way.

| After line | Add | Evidence |
|---|---|---|
| 80 | A call MUST contain at least one edit. A call MUST NOT contain more than 64 edits. A call MUST NOT contain more than 8 MiB of input. | internal/tool/patch/patch.go:39-40 `if len(input) > 8<<20 {` then `return tool.Plan{}, errors.New("patch: input is over 8 MiB; split the change into several calls")`; internal/tool/patch/patch.go:51 `if len(edits) == 0 \| |
| 86 | A call MUST NOT stage more than 64 MiB of file content. | internal/tool/patch/stage.go:88 `if stagedBytes > 64<<20 {`; stage.go:91 `return nil, errors.New("patch: this call stages over 64 MiB; split it into several calls")` |
| 90 | A successful apply MUST print `[PATH#DELETED]` for each deleted file. | internal/tool/patch/writer.go:448-449 `if file.deleted {` then `fmt.Fprintf(&out, "[%s#DELETED]\n", path)`. Add as a new paragraph after line 90. |

Use one word for one thing.

- Keep `snapshot`. Replace captured buffer at lines 52.
- Keep `snapshot`. Replace same buffer at lines 66.
- Keep `pre-image`. Replace pre-state at lines 78.
- Keep `post-image`. Replace post-state at lines 78.

Structure:

- Line 9 (Scope): confirmed, no change. N* is rejected by blockOpError (internal/tool/patch/hashline.go:119-120 and :293-294). Node, Symbol, and DefTag fail the generic verb check (hashline.go:122, apply_patch.go:61, anchor.go:69).
- Terms: post-state becomes post-image on line 78 so that the paragraph uses one word for each image. The audit did not list this change. The JSON key captures on line 68 stays as written.
- Confirmed without change: lines 17 BOM and CRLF (text.go:28-36, 71-74, 107-111; resolve.go:168-170), 38 and 40 (anchor.go:166-173, 206-209; resolve.go:63-72, 408-427), 54-58 (resolve.go:215-216, 232-242, 263-267), 60-62 (apply_patch.go:25-30, 49-51, 118-119, 142-151, 173-177, 185-202, 220; resolve.go:32-35), 70-74 (snapshot store.go:360-365, 118-119, 147-152, 208-209; mapping.go:3-27; store_test.go:21, 29; conformance_extra_test.go:73), 86-90 (writer.go:53-62, 67-86, 149, 164, 233-241, 284-305, 308-310, 384-410). Mask: the Tree rules of lines 62-72 are outside this list.

Rewrite these long sentences as written here.

| Line | Write |
|---|---|
| 68 | A `search` result carries `{"captures":[...]}` with at most 64 KiB of metadata. The `search` output MUST print `[PATH#TAG]`, `[PATH#VERSION]`, and `[PATH@ReadRef]` only for each capture in the `captures` array. |
| 68 | Restore MUST reject metadata with unknown, duplicate, or missing keys. Restore MUST reject trailing data. Restore MUST reject a blob that does not match the bytes. Restore MUST reject shown lines outside the bytes. Restore MUST reject a zero revision or epoch. |

Then make every sentence of the file follow rule 3 of this plan. Split each sentence that holds a semicolon outside code into two sentences. Keep every literal. The file has 2 style findings at commit `3c4b6f94`.

Proof: `uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec/edit-styles.md` prints `style findings: 0`, and `just verify` passes.

