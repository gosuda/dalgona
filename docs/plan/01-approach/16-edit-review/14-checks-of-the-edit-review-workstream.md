#### Checks of the edit-review workstream

- After each step, run its packages:
  - `go test -race -count=1 ./internal/textdiff` (Step 1)
  - `go test -race -count=1 ./internal/agent/event` (Step 2)
  - `go test -race -count=1 ./internal/config ./internal/app` (Step 3)
  - `go test -race -count=1 ./internal/agent -run 'EditReview|TurnReview|Recovery|Pack|SplitHunk|ReviewTakes|ReviewPart|ReviewStops|ReviewVerdict|RequireMasked|ReviewSummary|BuiltInSpeakers|NewHostRejectsUnknownFeedbackID|Feedback'` and `go test -race -count=1 ./internal/app -run EditJudge` and `go test -race -count=1 ./internal/decide -run RequestSize` (Steps 4, 4a, 5, 6, and 9)
  - `go test -race -count=1 ./internal/rpc -run 'Tracker|DalgonaMessagesHaveSchemas'` (Step 6)
  - `go test -race -count=1 ./internal/tui -run 'ReviewRow|ToolJudged|TurnJudged'` (Step 7)
  - `pnpm -C web exec vitest run src/lib/validation.test.ts src/lib/reducer.test.ts` (Step 8)
  - `go test -count=1 ./internal/agent -run 'EditReviewCorpus|BuiltInSpeakers'` (Step 9, no network)
- Before every commit: `just verify`.
- End to end at the host level: `TestEditReviewFeedbackFollowsToolResults` drives a real thread through the fake model, a real `patch` write, and the journal into the next request.
- End to end by hand, using the live-debugging recipe of product decision 16:
  1. Set `DALGONA_HOME` to a short temporary path. Use `model = "local/qwen-test"`, `approval = "yolo"`, `[edit_review]` with `enabled = true` and `feedback = ["lost_code"]`, `[approval_decider] decider = "llm:local/qwen-test"`, `[providers.local] base_url = "http://127.0.0.1:18080/v1"`, and `api_key_env = "LOCAL_LLM_KEY"`. The judge uses the `llm:` adapter with the model named in `[approval_decider] decider`. The default, `llm:smol`, resolves through the model roles (`judges.client`), so this run sets `[approval_decider] decider` to `llm:local/qwen-test` to reach this server. `yolo` skips approval decisions, so the decider key does not change them.
  2. Script the server's requests in this order:
     - Request 1, the working model, for the user message `edit main.go`: a `patch` call that deletes a function body in `main.go` and adds one line that matches no concurrency pattern.
     - Request 2, the edit judge, matched by a body that contains `"unclear_contract"`: `{"sound":0.04,"broken_syntax":0.01,"lost_code":0.97,"wrong_logic":0.02,"placeholder":0.01,"off_task":0.02,"needless_layer":0.03,"narration":0.02,"wasted_work":0.01,"unsafe_input":0.01,"unclear_contract":0.02}`. These are the ids the Go question set asks, and `unguarded_state` is not asked.
     - Request 3, the working model after the round's results: text `done`, with no tool call.
     - Request 4, the turn judge, matched by a body that contains `"incomplete"`: `{"sound":0.04,"incomplete":0.03}`.
     - Each reply must name exactly the ids asked for its request. The `llm:` adapter rejects a missing id and an unknown id (`parseLLMAnswers` and `readNamedLLMAnswer` in `internal/decide/llm.go`).
  3. Drive `just dalgona` with the omp `tui` tool.
  4. Expect a terminal row that contains `review`, then `Flagged: removed code`, then `main.go`. Expect the body of request 3 to contain `<edit-review call=`. Expect request 4 to be the turn judge, and expect the clean turn to print no row.
- Measurement, once credentials are present: `DALGONA_EDIT_REVIEW_EVAL=decisions:typesafe/jev-1.14 OPENROUTER_API_KEY=... go test -count=1 -v ./internal/agent -run TestEditReviewHeldOutPrecision`.

