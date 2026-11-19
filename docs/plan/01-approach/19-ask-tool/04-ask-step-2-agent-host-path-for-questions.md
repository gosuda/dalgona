#### Ask step 2. Agent host path for questions

Files:

- `internal/agent/types.go`:
  - Append `UpdateQuestion` after `UpdateExecOutput` (`:139`).
  - Add the type:

```go
type QuestionPrompt struct {
	RequestID  string
	CallID     string
	Questions  []asktool.Question
	Resolved   bool
	Resolution asktool.Outcome
}
```

  - Add the field `Question QuestionPrompt` to `Update` (`:155-164`).
  - Import `asktool "github.com/gosuda/dalgona/internal/tool/ask"`. The alias is required: package-level `ask[T]` exists at `thread.go:391`.
- `internal/agent/thread.go`:
  - Add the actor field `questions map[string]pendingQuestion` next to `approvals` (`:67`) and initialise it in the actor literal (`:133`).
  - `Subscribe` (`:508-514`) calls `a.subscribe(ch)` in place of `a.subs[ch] = struct{}{}`. The new `func (a *actor) subscribe(ch chan Update)` adds `ch`, then sends one `Update{Kind: UpdateQuestion, Thread: a.t.id, Question: p.prompt}` per open question; the channel is new, with the `subscriberBuffer` size (`thread.go:23`).
  - `unsubscribe` (`:522-529`) calls `a.declineQuestions()` when `len(a.subs) == 0` after the removal.
  - `Status` (`:543-550`) sets `Questions: len(a.questions)`.
- `internal/agent/seams.go`: add `Questions int` to `Status` (`:51-59`).
- `internal/agent/question.go` (new):

```go
type pendingQuestion struct {
	prompt  QuestionPrompt
	replies chan asktool.Reply
}

func (t *Thread) Ask(ctx context.Context, callID string, questions []asktool.Question) (asktool.Reply, error) {
	if raw, err := url.PathUnescape(callID); err == nil {
		callID = raw
	}
	if ctx.Err() != nil {
		return asktool.Reply{Outcome: asktool.OutcomeInterrupted}, nil
	}
	prompt := QuestionPrompt{RequestID: newID("req-"), CallID: callID, Questions: questions}
	replies := make(chan asktool.Reply, 1)
	offered, err := ask(t, func(a *actor) bool { return a.offerQuestion(prompt, replies) })
	if err != nil {
		return asktool.Reply{}, err
	}
	if !offered {
		return asktool.Reply{Outcome: asktool.OutcomeNoClient}, nil
	}
	reply := awaitQuestion(ctx, replies)
	t.closeQuestion(prompt, reply.Outcome)
	return reply, nil
}
```

  - `func (a *actor) offerQuestion(prompt QuestionPrompt, replies chan asktool.Reply) bool` returns false when `len(a.subs) == 0`. Otherwise it stores the question, publishes `Update{Kind: UpdateQuestion, Thread: a.t.id, Question: prompt}` and returns true.
  - `func awaitQuestion(ctx context.Context, replies <-chan asktool.Reply) asktool.Reply` selects on `replies` or `ctx.Done()`; the second gives `OutcomeInterrupted`.
  - `func (t *Thread) closeQuestion(prompt QuestionPrompt, outcome asktool.Outcome)` runs through `ask(t, ...)`. It deletes the request id and publishes the prompt with `Resolved=true` and `Resolution=outcome`. It ignores `ErrClosed`.
  - `func (t *Thread) AnswerQuestion(requestID string, reply asktool.Reply) error` follows `Thread.Answer` (`entry.go:172-181`). It looks up `a.questions`, deletes, sends, and returns `ErrUnknownRequest` wrapped with the id when the question is absent.
  - `declineQuestions` deletes every entry and sends `asktool.Reply{Outcome: asktool.OutcomeNoClient}`.
  - The URL unescape mirrors `childFor` (`spawn.go:71-74`), because tools receive `url.PathEscape(call.ID)` (`tools.go:46-52`). Before `offerQuestion`, every header, question, label, description and preview passes through `t.host.mask`. A masking error returns an error from `Ask` and offers no question.

Tests in `internal/agent/question_test.go`, package `agent`. They call `thread.Ask` directly on `newHarness(t).start()` (`harness_test.go:219,310`) and fail before this step because `Ask` does not exist.

- `TestAskReachesSubscriberAndReturnsAnswer`
- `TestAskWithoutSubscriberDeclines` (the reply is `OutcomeNoClient` and no update is published)
- `TestAskDeclinesWhenLastSubscriberLeaves`
- `TestAskStaysOpenWhileAnotherSubscriberRemains`
- `TestAskReplaysOpenQuestionToNewSubscriber`
- `TestAskInterruptResolvesInterrupted` (ctx cancel; subscribers receive `Resolved=true`, `Resolution=OutcomeInterrupted`)
- `TestAnswerQuestionFirstAnswerWins` (the second `AnswerQuestion` gives `ErrUnknownRequest`; the resolved update carries `OutcomeAnswered`)
- `TestStatusCountsOpenQuestions`

These use channels and a 10 s deadline, never sleeps, so they stay off the load-sensitive list.

Docs: `docs/spec/ask.md` gets `## Host path`, covering offer, replay, the last-subscriber decline, the interrupt and first-answer-wins.

- Lands after: Step 1 (the `ask` package types).

Anchor: Edsger W. Dijkstra.

