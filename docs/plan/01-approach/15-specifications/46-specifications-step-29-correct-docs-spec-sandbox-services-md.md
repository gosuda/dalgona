#### Specifications step 29. Correct `docs/spec/sandbox-services.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/spec/sandbox-services.md` in one commit. Line numbers are from commit `3c4b6f94`.

Replace these statements. The code is right and the text is stale.

| Line | Now | Write | Evidence |
|---|---|---|---|
| 13 | Its entry command MUST create `/data/workspace` and `/data/home`. It MUST link `/workspace` to `/data/workspace` and then sleep indefinitely. | Its entry command MUST be `sleep infinity`. After creation, the service MUST run `mkdir -p /data/workspace /data/home && ln -sfn /data/workspace /workspace` in the sandbox. | internal/sandboxsvc/modal/sdk.go:54 `Command: []string{"sleep", "infinity"},`; sdk.go:72 `if err := initializeWorkspace(ctx, box); err != nil {`; sdk.go:85 `box.Exec(ctx, []string{"sh", "-c", "mkdir -p /data/workspace /data/home && ln -sfn /data/workspace /wor |
| 17 | A finished sandbox MUST cause the service to create the next numbered sandbox on the same volume. | A finished sandbox MUST cause the service to create a replacement sandbox on the same volume. The replacement MUST start at `n` = 1. | internal/sandboxsvc/modal/service.go:79 `Sequence:    1,` in createBox; service.go:123 `box, current, err := s.createBox(ctx, volume, image)` in replace; service_test.go:103-105 pins two create requests, both with `Sequence: 1`. |
| 53 | Create, Attach, Destroy, and Connect MUST fail before any request with `cloudflare: invalid box name` when it does not. | Create, Attach, Destroy, and Connect MUST fail before any request when a box name does not match. The error text MUST contain `cloudflare: invalid box name`. | internal/sandboxsvc/cloudflare/cloudflare.go:142-143 `if !boxName.MatchString(id) {` / `return "", fmt.Errorf("cloudflare: invalid box name %q: must match %s", id, boxName)`; cloudflare.go:102 `u, err := s.boxURL(id)` runs before the request is built; cloudfla |
| 53 | For an authorized request whose box name breaks the rule, the worker MUST answer 400 `invalid box name`. | For an authorized request whose box name breaks the rule, the worker MUST answer 400. The response body MUST start with `invalid box name`. | deploy/cloudflare/src/index.ts:102-105 `if (route.invalidName) { return new Response(` / `"invalid box name: use letters, digits, _ or -, up to 64 chars, starting with a letter or digit",` / `{ status: 400 },` |
| 55 | The client MUST send `DALGONA_TOKEN` to the worker. | The client MUST send the worker secret, which is the value of `DALGONA_TOKEN` on the worker, as the bearer token. The client reads that value from `DALGONA_CLOUDFLARE_TOKEN`, or else from the `cloudflare` entry in `auth.json`. | internal/remote/remote.go:195 `token, err := b.token("cloudflare", "DALGONA_CLOUDFLARE_TOKEN")`; remote.go:202-210 `func (b *Binder) token` reads the environment variable first and then `b.storedKey(provider)`; internal/remote/remote_test.go:208-211 `t.Setenv( |

Add these statements. The code already behaves this way.

| After line | Add | Evidence |
|---|---|---|
| 7 | `MODAL_CONFIG_PATH` MUST replace `~/.modal.toml` as the config file when set. | module SDK /home/alpha/go/pkg/mod/github.com/modal-labs/modal-client/go@v0.11.1/config.go:102-103 `if configPath := os.Getenv("MODAL_CONFIG_PATH"); configPath != "" {` / `return configPath, nil`; config.go:110 `return fi |
| 13 | If workspace initialization fails, Create MUST terminate the new sandbox and return the error. | internal/sandboxsvc/modal/sdk.go:72-80 `finishCreate`: `initializeWorkspace` error, then `box.Terminate(cleanupCtx, &sdk.SandboxTerminateParams{Wait: true})` at sdk.go:75, then `return nil, "", err`. |
| 17 | If the replacement cannot be created, Attach MUST return an error and no identifier. | internal/sandboxsvc/modal/service.go:110 `return s.replace(ctx, id, volume)`; service.go:123-125 `box, current, err := s.createBox(ctx, volume, image)` / `if err != nil { return "", fmt.Errorf("modal: replace %s: %w", id |
| 19 | If the sandbox no longer exists, Destroy MUST skip termination and MUST still delete the named volume. | internal/sandboxsvc/modal/service.go:159-162 and 165-168 (gone errors); takes effect only with Spec-fix step 13 (`Modal Destroy deletes the volume when the sandbox is already gone`). |
| 49 | If Sandbox0 answers 404 to Destroy, Destroy MUST return an error that wraps `sandboxsvc.ErrGone`. | internal/sandboxsvc/sandbox0/service.go:120-125 `func (s *Service) Destroy`: `if apiErr, ok := errors.AsType[*sdk.APIError](err); ok && apiErr.StatusCode == http.StatusNotFound {` / `return fmt.Errorf("sandbox0: delete % |
| 53 | If the worker answers 404 to Destroy, Destroy MUST return an error that wraps `sandboxsvc.ErrGone`. | internal/sandboxsvc/cloudflare/cloudflare.go:53-54 `if resp.StatusCode == http.StatusNotFound {` / `return fmt.Errorf("cloudflare: delete %s: status 404: %w", id, sandboxsvc.ErrGone)`. Add as the last sentence of the box |
| 53 | The Cloudflare exec-server endpoint MUST connect to `/box/<name>`. The Cloudflare bridge endpoint MUST connect to `/box/<name>/bridge`. | internal/sandboxsvc/cloudflare/cloudflare.go:63-65 `path := "/"` / `path = "/bridge"`; cloudflare.go:116-120 websocketURL adds no path element when the suffix is "/"; cloudflare.go:141-146 boxURL builds `append([]string{ |

Use one word for one thing.

- Keep `box identifier`. Replace its identifier; the new identifier at lines 17.

Structure:

- Line 7 agrees with the module SDK: client.go:77-79 and client.go:143 (module cache, not in the repository). No change to the profile rule itself.
- Cross-file: deploy/cloudflare/README.md lines 5 and 8 repeat the `DALGONA_TOKEN` client claim that the replaced statement at line 55 corrects. Change those two lines of `deploy/cloudflare/README.md` in the same commit.

Then make every sentence of the file follow rule 3 of this plan. Split each sentence that holds a semicolon outside code into two sentences. Keep every literal. The file has 0 style findings at commit `3c4b6f94`.

Proof: `uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec/sandbox-services.md` prints `style findings: 0`, and `just verify` passes.

