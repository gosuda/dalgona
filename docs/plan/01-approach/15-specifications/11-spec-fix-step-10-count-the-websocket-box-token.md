#### Spec-fix step 10. Count the websocket box token in characters, not bytes
Anchor: Tony Hoare, Richard Feynman

The text is right and the code breaks it. `docs/spec/exec-server.md` line 56 says: "Websocket mode MUST require `DALGONA_BOX_TOKEN` with at least 32 characters." Keep that sentence.

- Where: `internal/runner/execserver.go:ExecServerMainWithBridge`.
- Change: At line 54 replace `len(os.Getenv(boxEnvironmentKey)) < 32` with `utf8.RuneCountInString(os.Getenv(boxEnvironmentKey)) < 32` and add the `unicode/utf8` import. Keep line 55 and its error text unchanged. This matches the web UI rule at internal/webui/webui.go:107.
- Failing-first test: `TestExecServerMain_requiresThirtyTwoCharacters` in `internal/runner/execserver_test.go`. Input: DALGONA_BOX_TOKEN = strings.Repeat("é", 16) (16 characters, 32 bytes); argv `--listen ws://127.0.0.1:99999 --root <t.TempDir()>` Expected: ExecServerMain returns 1 and stdout is exactly `exec-server: ws listen needs DALGONA_BOX_TOKEN (32+ characters)` plus a newline. Port 99999 makes the listener fail fast, so the current code prints a listen error and fails the assertion without blocking. The expected value comes from: Spec line 56 `at least 32 characters`; spec line 59 error literal; the same literal is pinned at execserver_test.go:36.
- Evidence at commit `3c4b6f94`: execserver.go:54 `if strings.HasPrefix(cfg.listen, "ws://") && len(os.Getenv(boxEnvironmentKey)) < 32 {` counts bytes. Chosen as fix_code because the spec states an auth-strength rule and the code accepts a 16-character token that the rule forbids; webui.go:107 already counts runes for the same rule.
- Proof: the test fails before the change and passes after it, and `just verify` passes.

