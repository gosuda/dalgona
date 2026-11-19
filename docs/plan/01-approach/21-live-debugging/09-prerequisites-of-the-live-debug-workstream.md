#### Prerequisites of the live-debug workstream

- Steps 1 and 2 depend on no other workstream. Step 3 depends on the ratchet workstream, Step 3, for the `dist` recipe it appends after. They land in phase 13 in the order 1, 2, 3.
- The sessions (step 4 rows) assume the interface workstreams have landed and documented the following in `docs/spec/tui.md` and `docs/spec/web.md`:
  - the newline key
  - the rewind gesture
  - the escape for a prompt that starts with `/`
  - the notification OSC
  - the `theme` setting (`auto | auto-soft | day | soft-day | night | soft-night`), the `icons` setting (`auto | unicode | nerd`), the `status_hide` setting, and the `DESIGN.md` tokens for Design record, Colour; Design record, Product marks (`DESIGN.md` is absent from the repo root at commit `3c4b6f94`) and the verbosity setting, both reachable from `/settings`
  - the ask tool name `ask` with `item/tool/requestUserInput`
  - `account/login/completed` consumption
  - the stability workstream's Retry-After rule (cross-phase decision 41): a countdown up to 300 s, and the messages `rate limited for <s> s, over the <limit> s wait limit` with `<limit>` 300 or 60
  - the ratchet tool argument schema (the ratchet workstream, Step 6) and the edit-review judge reply shape (the edit-review workstream)
  - the image-input workstream (image parts in chat-completions requests and `inputModalities`), needed by T25 and T46
  - The protocol workstream, Steps 8 and 12 (config keys `status_hide` and `notifications`, and `/login` for API-key providers)
  - The ask workstream, Steps 1 and 8; the edit-review workstream, Steps 7 and 8; the core-fixes workstream, Step 1a; and the protocol workstream, Step 13 (sandbox question)
- If a row's feature has not landed, the report marks it `Failed, defect` against that workstream, not this one.
- `TestLiveSessionTurnsExist` (Step 4) reads turn ids only from this workstream's spec sections.

