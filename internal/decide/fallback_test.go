package decide

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"io"
	"net/http"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/model"
)

type askFunc func(ctx context.Context, call int32) (Result, error)

type stubClient struct {
	calls atomic.Int32
	ask   askFunc
}

func (s *stubClient) Ask(ctx context.Context, _ jsontext.Value, _ map[string]Question) (Result, error) {
	return s.ask(ctx, s.calls.Add(1))
}

func answeringClient(modelName string) *stubClient {
	return &stubClient{ask: func(context.Context, int32) (Result, error) {
		return validAnswers(modelName), nil
	}}
}

func failingClient(err error) *stubClient {
	return &stubClient{ask: func(context.Context, int32) (Result, error) {
		return Result{}, err
	}}
}

func blockingClient() *stubClient {
	return &stubClient{ask: func(ctx context.Context, _ int32) (Result, error) {
		<-ctx.Done()
		return Result{}, ctx.Err()
	}}
}

func validAnswers(modelName string) Result {
	noul := 0.9
	return Result{
		Answers: map[string]Answer{"ok": {Noul: &noul}},
		Model:   modelName,
	}
}

func fallbackQuestions() map[string]Question {
	return map[string]Question{"ok": {Type: TypeNoul}}
}

func askState() jsontext.Value {
	return jsontext.Value(`{"task":"t"}`)
}

func checkFallbackSuccess(t *testing.T, result Result, err error, primaryModel string, primaryErr error) {
	t.Helper()
	if err != nil {
		t.Fatalf("Ask error = %v", err)
	}
	if result.Model != "smol-model" {
		t.Fatalf("answering model = %q, want smol-model", result.Model)
	}
	answer, ok := result.Answers["ok"]
	if !ok || answer.Noul == nil || *answer.Noul != 0.9 {
		t.Fatalf("answers = %+v, want ok noul 0.9", result.Answers)
	}
	if want := primaryModel + ": " + primaryErr.Error(); result.Fallback != want {
		t.Fatalf("fallback = %q, want %q", result.Fallback, want)
	}
}

type fallbackCase struct {
	name       string
	primaryErr error
	fast       bool
}

func runFallbackAfterPrimaryError(t *testing.T, test fallbackCase) {
	t.Helper()
	t.Parallel()
	var primary *stubClient
	if test.fast {
		primary = failingClient(test.primaryErr)
	} else {
		primary = blockingClient()
	}
	smol := answeringClient("smol-model")
	client := WithFallback(Named{Client: primary, Model: "primary-model"}, Named{Client: smol, Model: "smol-role"}, NewCooldowns(nil))
	result, err := client.Ask(t.Context(), askState(), fallbackQuestions())
	expectedErr := test.primaryErr
	if !test.fast {
		expectedErr = context.DeadlineExceeded
	}
	checkFallbackSuccess(t, result, err, "primary-model", expectedErr)
	if got := primary.calls.Load(); got != 1 {
		t.Fatalf("primary calls = %d, want 1", got)
	}
	if got := smol.calls.Load(); got != 1 {
		t.Fatalf("smol calls = %d, want 1", got)
	}
}

func TestFallbackAsksSmolAfterPrimaryError(t *testing.T) {
	t.Parallel()
	refusal := errAnswer(errors.New("decisions: refusal"))
	malformed := errAnswer(errors.New("llm: invalid JSON: not-json"))
	toolCall := errAnswer(errors.New("llm: model returned a tool call instead of JSON"))
	missing := errAnswer(errors.New("llm: invalid JSON: missing answers"))
	invalidRequest := errRequest(errors.New(`decide: encode request: question "ok": criteria are required`))
	tests := []fallbackCase{
		{name: "http 402", primaryErr: &HTTPError{Status: 402, Body: "payment required"}, fast: true},
		{name: "http 429 with retry-after 120", primaryErr: &HTTPError{Status: 429, RetryAfter: 120 * time.Second, Body: "rate limited"}, fast: true},
		{name: "provider http 503", primaryErr: &model.HTTPError{Status: 503, Err: errors.New("chatcompat: status 503: unavailable")}, fast: true},
		{name: "transport error", primaryErr: errors.New("dial tcp 127.0.0.1:443: connection refused"), fast: true},
		{name: "missing token", primaryErr: errors.New("decisions: token: secret store locked"), fast: true},
		{name: "attempt timeout", primaryErr: context.DeadlineExceeded},
		{name: "refusal", primaryErr: refusal, fast: true},
		{name: "malformed JSON", primaryErr: malformed, fast: true},
		{name: "tool-call answer", primaryErr: toolCall, fast: true},
		{name: "missing answer", primaryErr: missing, fast: true},
		{name: "invalid request", primaryErr: invalidRequest, fast: true},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			runFallbackAfterPrimaryError(t, test)
		})
	}
	t.Run("successful primary never asks smol", func(t *testing.T) {
		t.Parallel()
		primary := answeringClient("primary-model")
		smol := answeringClient("smol-model")
		client := WithFallback(Named{Client: primary, Model: "primary-model"}, Named{Client: smol, Model: "smol-role"}, NewCooldowns(nil))
		result, err := client.Ask(t.Context(), askState(), fallbackQuestions())
		if err != nil {
			t.Fatal(err)
		}
		if result.Model != "primary-model" || result.Fallback != "" {
			t.Fatalf("result = %+v, want primary model and empty fallback", result)
		}
		if got := smol.calls.Load(); got != 0 {
			t.Fatalf("smol calls = %d, want 0", got)
		}
	})
}

func TestFallbackCooldown(t *testing.T) {
	t.Parallel()
	base := time.Date(2026, 11, 15, 12, 0, 0, 0, time.UTC)
	current := base
	cooldowns := NewCooldowns(func() time.Time { return current })
	rateLimited := &HTTPError{Status: 429, RetryAfter: 120 * time.Second, Body: "rate limited"}
	primary := failingClient(rateLimited)
	smol := answeringClient("smol-model")
	client := WithFallback(Named{Client: primary, Model: "llm:p"}, Named{Client: smol, Model: "llm:smol"}, cooldowns)
	result, err := client.Ask(t.Context(), askState(), fallbackQuestions())
	checkFallbackSuccess(t, result, err, "llm:p", rateLimited)

	current = base.Add(119 * time.Second)
	result, err = client.Ask(t.Context(), askState(), fallbackQuestions())
	if err != nil {
		t.Fatal(err)
	}
	if got := primary.calls.Load(); got != 1 {
		t.Fatalf("primary calls during cooldown = %d, want 1", got)
	}
	if !strings.Contains(result.Fallback, "cooling down until ") {
		t.Fatalf("fallback during cooldown = %q", result.Fallback)
	}

	current = base.Add(121 * time.Second)
	if _, err := client.Ask(t.Context(), askState(), fallbackQuestions()); err != nil {
		t.Fatal(err)
	}
	if got := primary.calls.Load(); got != 2 {
		t.Fatalf("primary calls after cooldown = %d, want 2", got)
	}
	if len(cooldowns.until) != 1 {
		t.Fatalf("cooldown entries after retry = %d, want 1", len(cooldowns.until))
	}
	if until, ok := cooldowns.active("llm:p"); !ok || !until.Equal(base.Add(241*time.Second)) {
		t.Fatalf("cooldown after retry until = %v ok = %v, want %v", until, ok, base.Add(241*time.Second))
	}
}

func TestFallbackCooldownDefaultsToSixtySeconds(t *testing.T) {
	t.Parallel()
	base := time.Date(2026, 11, 15, 12, 0, 0, 0, time.UTC)
	current := base
	cooldowns := NewCooldowns(func() time.Time { return current })
	transport := errors.New("connection refused")
	primary := failingClient(transport)
	smol := answeringClient("smol-model")
	client := WithFallback(Named{Client: primary, Model: "llm:p"}, Named{Client: smol, Model: "llm:smol"}, cooldowns)
	if _, err := client.Ask(t.Context(), askState(), fallbackQuestions()); err != nil {
		t.Fatal(err)
	}
	until, ok := cooldowns.active("llm:p")
	if !ok || !until.Equal(base.Add(time.Minute)) {
		t.Fatalf("cooldown until = %v ok = %v, want %v", until, ok, base.Add(time.Minute))
	}
	current = base.Add(59 * time.Second)
	if _, err := client.Ask(t.Context(), askState(), fallbackQuestions()); err != nil {
		t.Fatal(err)
	}
	if got := primary.calls.Load(); got != 1 {
		t.Fatalf("primary calls at 59 s = %d, want 1", got)
	}
	current = base.Add(61 * time.Second)
	if _, err := client.Ask(t.Context(), askState(), fallbackQuestions()); err != nil {
		t.Fatal(err)
	}
	if got := primary.calls.Load(); got != 2 {
		t.Fatalf("primary calls at 61 s = %d, want 2", got)
	}
}

func TestFallbackCooldownKeepsLaterExpiryAndSeparateKeys(t *testing.T) {
	t.Parallel()
	base := time.Date(2026, 11, 15, 12, 0, 0, 0, time.UTC)
	cooldowns := NewCooldowns(func() time.Time { return base })
	cooldowns.set("llm:a", base.Add(120*time.Second))
	cooldowns.set("llm:a", base.Add(30*time.Second))
	if until, ok := cooldowns.active("llm:a"); !ok || !until.Equal(base.Add(120*time.Second)) {
		t.Fatalf("llm:a until = %v ok = %v, want the later 120 s expiry", until, ok)
	}
	if _, ok := cooldowns.active("llm:b"); ok {
		t.Fatal("llm:b is cooling down, want independent keys")
	}
}

func TestFallbackAnswerAndRequestCategoriesNeverCoolDown(t *testing.T) {
	t.Parallel()
	for _, err := range []error{
		errAnswer(errors.New("llm: invalid JSON: not-json")),
		errRequest(errors.New("decide: encode request: bad question")),
	} {
		primary := failingClient(err)
		smol := answeringClient("smol-model")
		cooldowns := NewCooldowns(nil)
		client := WithFallback(Named{Client: primary, Model: "llm:p"}, Named{Client: smol, Model: "llm:smol"}, cooldowns)
		for range 2 {
			if _, askErr := client.Ask(t.Context(), askState(), fallbackQuestions()); askErr != nil {
				t.Fatal(askErr)
			}
		}
		if got := primary.calls.Load(); got != 2 {
			t.Fatalf("primary calls = %d, want 2 with no cooldown", got)
		}
		if len(cooldowns.until) != 0 {
			t.Fatalf("cooldown entries = %d, want 0", len(cooldowns.until))
		}
	}
}

func TestFallbackTypedHTTPErrorsRetainRetryAfter(t *testing.T) {
	t.Parallel()
	provider := &model.HTTPError{Status: 429, RetryAfter: 200 * time.Second, Err: errors.New("chatcompat: status 429: rate limited")}
	primary := failingClient(provider)
	smol := answeringClient("smol-model")
	cooldowns := NewCooldowns(nil)
	client := WithFallback(Named{Client: primary, Model: "llm:p"}, Named{Client: smol, Model: "llm:smol"}, cooldowns)
	if _, err := client.Ask(t.Context(), askState(), fallbackQuestions()); err != nil {
		t.Fatal(err)
	}
	until, ok := cooldowns.active("llm:p")
	if !ok || time.Until(until) < 199*time.Second {
		t.Fatalf("cooldown until = %v ok = %v, want the 200 s retry-after", until, ok)
	}
}

func TestProviderHTTPFailureCarriesStatusAndRetryAfter(t *testing.T) {
	t.Parallel()
	resp := &http.Response{
		StatusCode: http.StatusTooManyRequests,
		Header:     http.Header{"Retry-After": {"200"}},
		Body:       io.NopCloser(strings.NewReader("rate limited\n")),
	}
	result := model.HTTPFailure("chatcompat", resp)
	httpErr, ok := errors.AsType[*model.HTTPError](result.Err)
	if !ok {
		t.Fatalf("error = %v, want *model.HTTPError", result.Err)
	}
	if httpErr.Status != http.StatusTooManyRequests {
		t.Fatalf("status = %d, want 429", httpErr.Status)
	}
	if httpErr.RetryAfter != 200*time.Second {
		t.Fatalf("retry after = %v, want 200 s", httpErr.RetryAfter)
	}
	if want := "chatcompat: status 429: rate limited"; result.Err.Error() != want {
		t.Fatalf("error text = %q, want %q", result.Err.Error(), want)
	}
}

func TestFallbackSameModelIsPrimary(t *testing.T) {
	t.Parallel()
	primary := failingClient(errors.New("boom"))
	smol := answeringClient("smol-model")
	cooldowns := NewCooldowns(nil)
	if got := WithFallback(Named{Client: primary, Model: "llm:m"}, Named{Client: smol, Model: "llm:m"}, cooldowns); got != Client(primary) {
		t.Fatal("equal identities wrapped the primary, want the primary client unchanged")
	}
	if got := WithFallback(Named{Client: primary, Model: "llm:p"}, Named{}, cooldowns); got != Client(primary) {
		t.Fatal("missing smol client wrapped the primary, want the primary client unchanged")
	}
	if got := WithFallback(Named{Client: primary, Model: "llm:m"}, Named{Client: smol, Model: "decisions:m"}, cooldowns); got == Client(primary) {
		t.Fatal("different adapters with equal model text were treated as identical")
	}
	client := WithFallback(Named{Client: primary, Model: ""}, Named{Client: smol, Model: ""}, cooldowns)
	if _, err := client.Ask(t.Context(), askState(), fallbackQuestions()); err != nil {
		t.Fatal(err)
	}
	if got := smol.calls.Load(); got != 1 {
		t.Fatalf("smol calls for unknown identities = %d, want 1", got)
	}
}

func TestFallbackBothFail(t *testing.T) {
	t.Parallel()
	rateLimited := &HTTPError{Status: 429, RetryAfter: 120 * time.Second, Body: "rate limited"}
	unavailable := &model.HTTPError{Status: 503, Err: errors.New("chatcompat: status 503: unavailable")}
	primary := failingClient(rateLimited)
	smol := failingClient(unavailable)
	cooldowns := NewCooldowns(nil)
	client := WithFallback(Named{Client: primary, Model: "llm:p"}, Named{Client: smol, Model: "llm:smol"}, cooldowns)
	result, err := client.Ask(t.Context(), askState(), fallbackQuestions())
	if result.Model != "" || result.Fallback != "" || len(result.Answers) != 0 {
		t.Fatalf("result = %+v, want the zero result", result)
	}
	requireBothFailCauses(t, err, rateLimited, unavailable)
	if want := "decisions: http 429: rate limited; fallback llm:smol: chatcompat: status 503: unavailable"; err.Error() != want {
		t.Fatalf("error text = %q, want %q", err.Error(), want)
	}
	if _, ok := cooldowns.active("llm:smol"); ok {
		t.Fatal("cooldown was keyed to the smol model")
	}

	if _, err := client.Ask(t.Context(), askState(), fallbackQuestions()); err == nil {
		t.Fatal("second Ask succeeded while cooling down with a failing smol")
	}
	if got := primary.calls.Load(); got != 1 {
		t.Fatalf("primary calls after cooldown skip = %d, want 1", got)
	}
}

func requireBothFailCauses(t *testing.T, err, rateLimited, unavailable error) {
	t.Helper()
	fallbackErr, ok := errors.AsType[*FallbackError](err)
	if !ok {
		t.Fatalf("error = %v, want *FallbackError", err)
	}
	if !errors.Is(err, rateLimited) || !errors.Is(err, unavailable) {
		t.Fatalf("error = %v, want both causes reachable", err)
	}
	if _, ok := errors.AsType[*HTTPError](err); !ok {
		t.Fatal("primary HTTP cause is not reachable through errors.As")
	}
	if _, ok := errors.AsType[*model.HTTPError](fallbackErr.SmolErr); !ok {
		t.Fatal("smol HTTP cause is not reachable through errors.As")
	}
}

func TestFallbackCooldownSkipReportsCooldownCause(t *testing.T) {
	t.Parallel()
	rateLimited := &HTTPError{Status: 429, RetryAfter: 120 * time.Second, Body: "rate limited"}
	unavailable := &model.HTTPError{Status: 503, Err: errors.New("chatcompat: status 503: unavailable")}
	primary := failingClient(rateLimited)
	smol := failingClient(unavailable)
	cooldowns := NewCooldowns(nil)
	client := WithFallback(Named{Client: primary, Model: "llm:p"}, Named{Client: smol, Model: "llm:smol"}, cooldowns)
	_, firstErr := client.Ask(t.Context(), askState(), fallbackQuestions())
	if _, ok := errors.AsType[*FallbackError](firstErr); !ok {
		t.Fatalf("first error = %v, want *FallbackError", firstErr)
	}
	if !errors.Is(firstErr, rateLimited) {
		t.Fatalf("first error = %v, want the primary 429 cause", firstErr)
	}
	_, err := client.Ask(t.Context(), askState(), fallbackQuestions())
	fallbackErr, ok := errors.AsType[*FallbackError](err)
	if !ok {
		t.Fatalf("error = %v, want *FallbackError", err)
	}
	if _, ok := errors.AsType[*CooldownError](fallbackErr.PrimaryErr); !ok {
		t.Fatalf("primary cause = %v, want *CooldownError", fallbackErr.PrimaryErr)
	}
}

func TestFallbackHonoursCallerCancel(t *testing.T) {
	t.Parallel()
	t.Run("already canceled parent asks no model", runCanceledParentAsksNoModel)
	t.Run("cancel during primary asks no smol and cools nothing", runCancelDuringPrimaryAsksNoSmol)
	t.Run("cancel during smol keeps both causes", runCancelDuringSmolKeepsBothCauses)
	t.Run("shorter caller deadline limits the smol budget", runShorterCallerDeadlineLimitsSmolBudget)
	t.Run("smol answer after cancellation is not accepted", runSmolAnswerAfterCancellationNotAccepted)
}

func runCanceledParentAsksNoModel(t *testing.T) {
	t.Helper()
	t.Parallel()
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	primary := answeringClient("primary-model")
	smol := answeringClient("smol-model")
	client := WithFallback(Named{Client: primary, Model: "llm:p"}, Named{Client: smol, Model: "llm:smol"}, NewCooldowns(nil))
	if _, err := client.Ask(ctx, askState(), fallbackQuestions()); !errors.Is(err, context.Canceled) {
		t.Fatalf("error = %v, want context.Canceled", err)
	}
	if primary.calls.Load() != 0 || smol.calls.Load() != 0 {
		t.Fatal("a model was asked after parent cancellation")
	}
}

func runCancelDuringPrimaryAsksNoSmol(t *testing.T) {
	t.Helper()
	t.Parallel()
	ctx, cancel := context.WithCancel(context.Background())
	primary := &stubClient{ask: func(ctx context.Context, _ int32) (Result, error) {
		<-ctx.Done()
		return Result{}, ctx.Err()
	}}
	smol := answeringClient("smol-model")
	cooldowns := NewCooldowns(nil)
	client := WithFallback(Named{Client: primary, Model: "llm:p"}, Named{Client: smol, Model: "llm:smol"}, cooldowns)
	askDone := make(chan error, 1)
	go func() {
		_, err := client.Ask(ctx, askState(), fallbackQuestions())
		askDone <- err
	}()
	cancel()
	if err := <-askDone; !errors.Is(err, context.Canceled) {
		t.Fatalf("error = %v, want context.Canceled", err)
	}
	if got := smol.calls.Load(); got != 0 {
		t.Fatalf("smol calls = %d, want 0", got)
	}
	if len(cooldowns.until) != 0 {
		t.Fatalf("cooldown entries = %d, want 0", len(cooldowns.until))
	}
}

func runCancelDuringSmolKeepsBothCauses(t *testing.T) {
	t.Helper()
	t.Parallel()
	ctx, cancel := context.WithCancel(context.Background())
	primary := failingClient(errors.New("dial tcp: connection refused"))
	started := make(chan struct{})
	smol := &stubClient{ask: func(ctx context.Context, _ int32) (Result, error) {
		close(started)
		<-ctx.Done()
		return Result{}, ctx.Err()
	}}
	client := WithFallback(Named{Client: primary, Model: "llm:p"}, Named{Client: smol, Model: "llm:smol"}, NewCooldowns(nil))
	result := make(chan error)
	go func() {
		_, err := client.Ask(ctx, askState(), fallbackQuestions())
		result <- err
	}()
	<-started
	cancel()
	err := <-result
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("error = %v, want context.Canceled reachable", err)
	}
	fallbackErr, ok := errors.AsType[*FallbackError](err)
	if !ok {
		t.Fatalf("error = %v, want *FallbackError", err)
	}
	if fallbackErr.PrimaryErr == nil {
		t.Fatal("primary cause was dropped")
	}
}

func runShorterCallerDeadlineLimitsSmolBudget(t *testing.T) {
	t.Helper()
	t.Parallel()
	ctx, cancel := context.WithTimeout(context.Background(), 50*time.Millisecond)
	defer cancel()
	primary := failingClient(errors.New("dial tcp: connection refused"))
	smol := blockingClient()
	client := WithFallback(Named{Client: primary, Model: "llm:p"}, Named{Client: smol, Model: "llm:smol"}, NewCooldowns(nil))
	_, err := client.Ask(ctx, askState(), fallbackQuestions())
	if !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("error = %v, want context.DeadlineExceeded", err)
	}
}

func runSmolAnswerAfterCancellationNotAccepted(t *testing.T) {
	t.Helper()
	t.Parallel()
	ctx, cancel := context.WithCancel(context.Background())
	primary := failingClient(errors.New("dial tcp: connection refused"))
	smol := &stubClient{ask: func(context.Context, int32) (Result, error) {
		cancel()
		return validAnswers("smol-model"), nil
	}}
	client := WithFallback(Named{Client: primary, Model: "llm:p"}, Named{Client: smol, Model: "llm:smol"}, NewCooldowns(nil))
	result, err := client.Ask(ctx, askState(), fallbackQuestions())
	if result.Model != "" || result.Fallback != "" || len(result.Answers) != 0 {
		t.Fatalf("result = %+v, want the zero result", result)
	}
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("error = %v, want context.Canceled reachable", err)
	}
}

func TestFallbackSmolReceivesFreshAttemptBudget(t *testing.T) {
	t.Parallel()
	primaryArrival := make(chan time.Time, 1)
	primary := &stubClient{ask: func(ctx context.Context, _ int32) (Result, error) {
		primaryArrival <- time.Now()
		<-ctx.Done()
		return Result{}, ctx.Err()
	}}
	smolDeadline := make(chan time.Time, 1)
	smol := &stubClient{ask: func(ctx context.Context, _ int32) (Result, error) {
		deadline, ok := ctx.Deadline()
		if !ok {
			close(smolDeadline)
			return Result{}, errors.New("smol attempt has no deadline")
		}
		smolDeadline <- deadline
		return validAnswers("smol-model"), nil
	}}
	client := WithFallback(Named{Client: primary, Model: "llm:p"}, Named{Client: smol, Model: "llm:smol"}, NewCooldowns(nil))
	done := make(chan error, 1)
	go func() {
		_, err := client.Ask(context.Background(), askState(), fallbackQuestions())
		done <- err
	}()
	arrival := <-primaryArrival
	if err := <-done; err != nil {
		t.Fatal(err)
	}
	deadline := <-smolDeadline
	budget := time.Until(deadline)
	if budget < 7*time.Second {
		t.Fatalf("smol budget = %v, want approximately eight fresh seconds", budget)
	}
	if spent := time.Since(arrival); spent < 7*time.Second {
		t.Fatalf("primary attempt spent %v, want the full attempt timeout", spent)
	}
}

func TestFallbackConcurrentCallsShareCooldownState(t *testing.T) {
	t.Parallel()
	rateLimited := &HTTPError{Status: 429, RetryAfter: 120 * time.Second, Body: "rate limited"}
	primary := failingClient(rateLimited)
	smol := answeringClient("smol-model")
	cooldowns := NewCooldowns(nil)
	client := WithFallback(Named{Client: primary, Model: "llm:p"}, Named{Client: smol, Model: "llm:smol"}, cooldowns)
	var group sync.WaitGroup
	for range 8 {
		group.Go(func() {
			if _, err := client.Ask(context.Background(), askState(), fallbackQuestions()); err != nil {
				t.Error(err)
			}
		})
	}
	group.Wait()
	if !t.Failed() {
		if _, ok := cooldowns.active("llm:p"); !ok {
			t.Fatal("shared cooldown was not recorded")
		}
	}
}
