#### Web step 29. Model panel with presets

- New `panels/ModelPanel.svelte`.
  - A `Presets` group comes first, with one radio per role (`default`, `smol`, `slow`) for each model whose `presets` contains that role. Each is a radio showing `name · model · effort`.
  - Then `Models`: a radio list from `model/list` with description, and an effort radio group from `efforts`.
  - Submit `Use model` calls `config/batchWrite` with `{ edits: [{ keyPath: "model", value, mergeStrategy: "replace" }, { keyPath: "model_reasoning_effort", value: effort, mergeStrategy: "replace" }] }`.
  - Validation: the model must be in the list, and the effort in its `efforts`. Errors: `Choose a model.` and `Choose an effort for ${model}.`
  - Empty state: `No models. Log in to a provider to list its models.` with a `Log in` button that opens the login panel.
  - On success, reload `config/read` and close.
- Removed. `ModelPicker.svelte`, `changeModel` and its `config/value/write` call. The status band model segment opens this panel.
- Test. `ModelPanel.dom.test.ts`:
  - `writes model and effort in one batch`
  - `selecting a preset fills model and effort`
  - `the empty state leads to login`
  - Fails before: `No models available` is a dead end (`ModelPicker.svelte:24`).
- Spec (`docs/spec/web.md`). `### Commands and panels`, model panel row.
- Lands after: Steps 28 and 31 (`LoginPanel` is needed by the empty state).

Anchor: Herbert Simon.

