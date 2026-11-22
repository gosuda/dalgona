## Verification

These are required implementation checks, not a report that they have passed.
The [native and input workstream](01-approach/30-native-client-and-input.md) and [native verification](../dalgona/spec/native-verification.md) extend the existing product gates.

1. Run `just verify` before every commit under `AGENTS.md`. It covers Go formatting, vet, lint, race tests, vulnerability checks, shared models, specifications, links, web, and Cloudflare.
2. Report each actual result. A host-terminated tool, unavailable sandbox, skipped dependency, or unrun native check is not a pass.
3. Use behavior tests with independent expected results. Apply the required mutation probes to implementation tests. Source assertions and copied snapshots do not prove behavior.
4. Run `just spec-style` and `just docs-links`. Check new links in amended plan fragments separately because the repository link checker excludes those fragments.
5. Preserve the existing journal, recovery, approval, edit, code-tool, and hashline conformance gates. Do not weaken them to admit a new client.
6. Keep existing TUI dock geometry, scrollback, keyboard, and byte-stream behavior tests. Add exact Unicode preservation, source/display mappings, private-use chip separation, and declared width-profile evidence.
7. Run the web build, Biome, Vitest, and Svelte checks. Test real production editor and request adapters, including UTF-16 conversions and corrected responses after the first send.
8. Use the shared input vectors and pinned upstream grapheme corpus for GUI, TUI, web, and daemon. Cover limits, malformed encodings, atomic rejection, undo, recall, and round trips.
9. Verify Korean, Japanese, and Chinese composition with actual input methods on every declared native platform and browser matrix. Name terminal/IME pairs separately.
10. Verify search eligibility against the daemon's term rule and reject blank required model question fields without rewriting accepted source values.
11. Test snapshot cuts against pending coalescing, delayed replies, concurrent thread snapshots, and client-side buffering. Every accepted source byte appears once.
12. Test direct key login without callbacks and validate MCP runtime provenance across source collisions and changed inventory versions.
13. Test shared workspace ownership, explicit release, busy dependencies, stale operations, and more than eight sequential open/close cycles.
14. Use actual PTYs and ConPTY for ordered resize replay, read-only attachments, checkpoint recovery, final output after Stop, truncation, detach, and retention expiry.
15. Enumerate every C01-C51 component workflow. Verify source-preserving document selection, bounded figures and math, image handling, workspace authority, and complete export.
16. Run dedicated native build, race, platform, accessibility, packaging, and performance gates. Keep headless and remote-box builds independent of graphics dependencies.
17. Run the existing scripted live sessions after the relevant implementation gates. Record each observed result and reproduce failures before adding a behavioral regression test.
18. Distinguish local checks, desktop tests, live-service tests, and remaining outside checks in the delivery report.

A documentation-only amendment runs applicable documentation checks and the required repository verification attempt.
It does not claim runtime input, desktop, IME, or protocol acceptance.
The implementation is complete only when every applicable release gate has evidence.

