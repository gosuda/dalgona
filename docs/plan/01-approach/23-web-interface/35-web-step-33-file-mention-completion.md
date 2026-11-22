#### Web step 33. File mention completion

Completions follow [Input text](../../../dal/spec/input-text.md).
Only ASCII `@` triggers completion. Bind results to endpoint, thread, field, draft revision, query, and selection.
Discard stale results and defer replacements during composition.
Convert ranges explicitly and replace only complete grapheme clusters. Preserve the chosen path exactly in one undoable edit.

- Trigger. In `Composer.svelte`, an `@` at the start of the text or after whitespace starts a query that runs until the next whitespace.
- Search. Debounce 120 ms, then call `fuzzyFileSearch {query, roots: [thread.cwd], cancellationToken: "composer"}`.
- List. `CompletionList` shows up to 20 `file_name` and `path` entries. Enter or Tab inserts `@${path} `.
- Parser. `validation.ts` adds `parseFuzzyFileSearchResponse` and a conformance row (`FuzzyFileSearchResponse.json`).
- Test. `Composer.dom.test.ts`: `@ opens file completion and Enter inserts the path`. Fails before: there is no completion.
- Spec (`docs/spec/web.md`). `### Composer`: mentions.
- Lands after: Steps 27 and 28 (the keys table and `CompletionList`).

Anchor: Claude Shannon.

