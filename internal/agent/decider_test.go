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

func (s *stubAskClient) Ask(_ context.Context, _ jsontext.Value, questions map[string]decide.Question) (map[string]decide.Answer, error) {
	s.calls++
	out := make(map[string]decide.Answer, len(questions))
	for name := range questions {
		value := s.noul
		out[name] = decide.Answer{Noul: &value}
	}
	return out, nil
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
}
