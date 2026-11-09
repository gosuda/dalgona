package event_test

import (
	"encoding/json/jsontext"
	"reflect"
	"slices"
	"strconv"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
)

func TestFoldDoesNotMutateInput(t *testing.T) {
	t.Parallel()
	input := event.State{
		TurnID:       "t",
		PendingCalls: map[string]event.ToolCall{"c": {ID: "c", Name: "read", Args: jsontext.Value(`{}`)}},
	}
	before := event.State{
		TurnID:       "t",
		PendingCalls: map[string]event.ToolCall{"c": {ID: "c", Name: "read", Args: jsontext.Value(`{}`)}},
	}
	insert := event.AssistantMessage{
		ItemID:     "a",
		TurnID:     "t",
		StopReason: "tool_calls",
		ToolCalls:  []event.ToolCall{{ID: "fresh", Name: "read", Args: jsontext.Value(`{}`)}},
	}
	next, err := event.Fold(input, 1, insert)
	if err != nil {
		t.Fatalf("Fold: %v", err)
	}
	if !reflect.DeepEqual(input, before) {
		t.Fatal("Fold mutated its input state on the success path")
	}
	if _, ok := next.PendingCalls["fresh"]; !ok {
		t.Fatal("Fold dropped the assistant call from the returned state")
	}
}

func replayTurns(base event.State, turns int) (event.State, error) {
	acc := base.Clone()
	seq := uint64(0)
	for n := range turns {
		id := "replay-turn-" + strconv.Itoa(n)
		seq++
		if err := acc.Apply(seq, event.TurnStarted{TurnID: id}); err != nil {
			return acc, err
		}
		seq++
		if err := acc.Apply(seq, event.TurnCompleted{TurnID: id, Status: "completed"}); err != nil {
			return acc, err
		}
	}
	return acc, nil
}

func replayAllocs(t *testing.T, base event.State, turns int) []float64 {
	t.Helper()
	const samples = 25
	vals := make([]float64, 0, samples)
	for range samples {
		vals = append(vals, testing.AllocsPerRun(10, func() {
			if _, err := replayTurns(base, turns); err != nil {
				t.Fatalf("replay: %v", err)
			}
		}))
	}
	slices.Sort(vals)
	return vals
}

func TestReplayAllocationsScaleLinearly(t *testing.T) {
	base := event.State{}
	small := replayAllocs(t, base, 200)
	large := replayAllocs(t, base, 800)
	percentile := func(vals []float64, p float64) float64 { return vals[min(len(vals)-1, int(p*float64(len(vals))))] }
	for _, p := range []float64{0.50, 0.95, 0.99} {
		got, want := percentile(large, p), percentile(small, p)
		t.Logf("replay allocs p%.0f: 200 turns=%.1f 800 turns=%.1f ratio=%.2f", p*100, want, got, got/want)
		if want == 0 || got == 0 {
			t.Fatalf("replay allocs p%.0f = %.1f and %.1f, want nonzero samples", p*100, want, got)
		}
		if ratio := got / want; ratio > 6 {
			t.Fatalf("replay allocs p%.0f grew %.1fx for 4x events, want below 6x", p*100, ratio)
		}
	}
}

func BenchmarkFoldReplay(b *testing.B) {
	for _, turns := range []int{100, 400, 1600} {
		b.Run(strconv.Itoa(turns), func(b *testing.B) {
			base := event.State{}
			b.ResetTimer()
			for b.Loop() {
				if _, err := replayTurns(base, turns); err != nil {
					b.Fatal(err)
				}
			}
		})
	}
}
