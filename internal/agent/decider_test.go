package agent

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"testing"

	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/decide"
)

type stubAskClient struct {
	noul  float64
	calls int
}

func (s *stubAskClient) Ask(_ context.Context, _ jsontext.Value, questions map[string]decide.Question) (decide.Result, error) {
	s.calls++
	out := make(map[string]decide.Answer, len(questions))
	for name := range questions {
		value := s.noul
		out[name] = decide.Answer{Noul: &value}
	}
	return decide.Result{Answers: out, Model: "stub-model"}, nil
}

func TestFactoryDeciderPropagatesResolutionError(t *testing.T) {
	boom := errors.New("no such role")
	d := NewFactoryDecider(func(context.Context, string) (decide.Client, string, error) {
		return nil, "", boom
	}, 0.9, config.ProductDalgona)
	if _, err := d.Decide(t.Context(), Action{Thread: "thread-x"}); !errors.Is(err, boom) {
		t.Fatalf("Decide error = %v, want %v", err, boom)
	}
}

func TestFactoryDeciderScoresThroughClientDecider(t *testing.T) {
	stub := &stubAskClient{noul: 0.95}
	var gotThread string
	d := NewFactoryDecider(func(_ context.Context, threadID string) (decide.Client, string, error) {
		gotThread = threadID
		return stub, "stub-model", nil
	}, 0.9, config.ProductDalgona)
	got, err := d.Decide(t.Context(), Action{Thread: "thread-x"})
	if err != nil {
		t.Fatalf("Decide error = %v", err)
	}
	if gotThread != "thread-x" {
		t.Fatalf("factory thread = %q, want thread-x", gotThread)
	}
	if stub.calls != 1 {
		t.Fatalf("client Ask calls = %d, want 1", stub.calls)
	}
	if !got.Allow || got.By != "decider:stub-model" {
		t.Fatalf("decision = %+v, want an allow by decider:stub-model", got)
	}
	for _, tc := range []struct {
		score float64
		allow bool
	}{
		{0.9, true},
		{0.8999, false},
	} {
		stub.noul = tc.score
		got, err := d.Decide(t.Context(), Action{Thread: "thread-x"})
		if err != nil {
			t.Fatalf("Decide at score %v: %v", tc.score, err)
		}
		if got.Allow != tc.allow {
			t.Fatalf("score %v: Allow = %v, want %v", tc.score, got.Allow, tc.allow)
		}
	}
}

type fallbackAskClient struct {
	noul float64
}

func (s fallbackAskClient) Ask(_ context.Context, _ jsontext.Value, questions map[string]decide.Question) (decide.Result, error) {
	out := make(map[string]decide.Answer, len(questions))
	for name := range questions {
		value := s.noul
		out[name] = decide.Answer{Noul: &value}
	}
	return decide.Result{Answers: out, Model: "smol-model", Fallback: "llm:primary: decisions: http 429: rate limited"}, nil
}

func TestDeciderNamesFallbackModel(t *testing.T) {
	for _, tc := range []struct {
		noul   float64
		verb   string
		scores string
	}{
		{0.95, "allowed", "reversible=0.95 serves_task=0.95"},
		{0.20, "declined", "reversible=0.2 serves_task=0.2"},
	} {
		d := NewDecider(fallbackAskClient{noul: tc.noul}, "llm:primary", 0.9, config.ProductDalgona)
		got, err := d.Decide(t.Context(), Action{Thread: "thread-x", Task: "ship it"})
		if err != nil {
			t.Fatalf("Decide at %v: %v", tc.noul, err)
		}
		if got.Allow != (tc.verb == "allowed") {
			t.Fatalf("score %v: Allow = %v", tc.noul, got.Allow)
		}
		if got.By != "decider:smol-model" {
			t.Fatalf("By = %q, want decider:smol-model", got.By)
		}
		want := tc.verb + " by decider smol-model (fallback from llm:primary: decisions: http 429: rate limited): " + tc.scores + " (threshold 0.9)"
		if got.Reason != want {
			t.Fatalf("Reason = %q, want %q", got.Reason, want)
		}
	}
}
