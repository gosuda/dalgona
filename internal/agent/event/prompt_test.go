package event_test

import (
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
)

func TestPromptGenerationReferencesSurviveUpdates(t *testing.T) {
	t.Parallel()
	state, err := foldEvents(t,
		event.PromptBase{Generation: 1, Content: "frozen base"},
		event.MemoryUpdated{Generation: 1, Content: "old memory"},
		event.TurnStarted{TurnID: "t"},
		event.PromptBound{TurnID: "t", BaseGeneration: 1, MemoryGeneration: 1},
		event.PromptBase{Generation: 2, Content: "new base"},
		event.MemoryUpdated{Generation: 2, Content: "new memory"},
	)
	if err != nil {
		t.Fatal(err)
	}
	if state.PromptBases[1] != "frozen base" || state.MemoryVersions[1] != "old memory" {
		t.Fatal("an open turn lost its referenced generations")
	}
	clone := state.Clone()
	clone.PromptBases[1] = "mutated clone"
	clone.MemoryVersions[1] = "mutated clone"
	delete(clone.PromptBindings, "t")
	if state.PromptBases[1] != "frozen base" || state.MemoryVersions[1] != "old memory" || state.PromptBindings["t"].TurnID != "t" {
		t.Fatal("cloned prompt state aliases the original")
	}
}

func TestPromptFoldRejectsUnknownAndChangedBindings(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name string
		tail []event.Event
	}{
		{name: "base gap", tail: []event.Event{event.PromptBase{Generation: 3, Content: "gap"}}},
		{name: "memory gap", tail: []event.Event{event.MemoryUpdated{Generation: 2, Content: "gap"}}},
		{name: "unknown base", tail: []event.Event{event.PromptBound{TurnID: "t", BaseGeneration: 2}}},
		{name: "unknown memory", tail: []event.Event{event.PromptBound{TurnID: "t", BaseGeneration: 1, MemoryGeneration: 1}}},
		{name: "changed binding", tail: []event.Event{
			event.PromptBound{TurnID: "t", BaseGeneration: 1},
			event.MemoryUpdated{Generation: 1, Content: "new"},
			event.PromptBound{TurnID: "t", BaseGeneration: 1, MemoryGeneration: 1},
		}},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			prefix := []event.Event{event.PromptBase{Generation: 1, Content: "base"}, event.TurnStarted{TurnID: "t"}}
			if _, err := foldEvents(t, append(prefix, test.tail...)...); err == nil {
				t.Fatal("invalid generation reference was accepted")
			}
		})
	}
	if _, err := foldEvents(t, event.PromptBase{Generation: 1, Content: "base"}, event.TurnStarted{TurnID: "t"},
		event.PromptBound{TurnID: "t", BaseGeneration: 1}, event.PromptBound{TurnID: "t", BaseGeneration: 1}); err != nil {
		t.Fatalf("identical binding with empty memory was rejected: %v", err)
	}
}
