package event_test

import (
	"reflect"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
)

const (
	ceilingChildJSON = `{"type":"child.spawned","call_id":"c1","child_id":"k1","runner":"local","tools":["exec","read","yield"]}`
	legacyChildJSON  = `{"type":"child.spawned","call_id":"c1","child_id":"k1","runner":"local"}`
)

func TestChildSpawnedToolsJournalForm(t *testing.T) {
	t.Parallel()
	spawned := event.ChildSpawned{CallID: "c1", ChildID: "k1", Runner: "local", Tools: []string{"exec", "read", "yield"}}
	encoded, err := event.Encode(spawned)
	if err != nil {
		t.Fatal(err)
	}
	if string(encoded) != ceilingChildJSON {
		t.Fatalf("encoded child.spawned = %s, want %s", encoded, ceilingChildJSON)
	}
	decoded, err := event.Decode([]byte(ceilingChildJSON))
	if err != nil {
		t.Fatal(err)
	}
	if got, ok := decoded.(event.ChildSpawned); !ok || !reflect.DeepEqual(got, spawned) {
		t.Fatalf("decoded child.spawned = %#v, want %#v", decoded, spawned)
	}
}

func TestChildSpawnedWithoutToolsDecodesAndFolds(t *testing.T) {
	t.Parallel()
	decoded, err := event.Decode([]byte(legacyChildJSON))
	if err != nil {
		t.Fatalf("legacy child.spawned: %v", err)
	}
	legacy, ok := decoded.(event.ChildSpawned)
	if !ok || len(legacy.Tools) != 0 {
		t.Fatalf("legacy child.spawned = %#v, want no tools", decoded)
	}
	encoded, err := event.Encode(legacy)
	if err != nil {
		t.Fatal(err)
	}
	if string(encoded) != legacyChildJSON {
		t.Fatalf("legacy child.spawned re-encoded as %s, want %s", encoded, legacyChildJSON)
	}
	state, err := foldEvents(t,
		event.SessionCreated{ID: "s", Cwd: "/work", Model: "p/m", Approval: "auto", Sandbox: "workspace-write", Runner: "local"},
		event.TurnStarted{TurnID: "t"},
		event.ToolStarted{CallID: "c1", TurnID: "t", Tool: "task", Class: "agent", Args: []byte(`{}`)},
		legacy,
	)
	if err != nil {
		t.Fatalf("fold legacy child.spawned: %v", err)
	}
	if got := state.Children["k1"]; got.CallID != "c1" || got.Runner != "local" {
		t.Fatalf("folded child = %+v", got)
	}
}

func TestChildSpawnedRejectsMalformedTools(t *testing.T) {
	t.Parallel()
	base := event.ChildSpawned{CallID: "c1", ChildID: "k1", Runner: "local"}
	cases := []struct {
		name  string
		tools []string
		want  string
	}{
		{"empty name", []string{"read", ""}, "tools[1] must not be empty"},
		{"unsorted", []string{"read", "exec"}, `tools must be sorted and unique: "exec" follows "read"`},
		{"duplicate", []string{"read", "read"}, `tools must be sorted and unique: "read" follows "read"`},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			bad := base
			bad.Tools = tc.tools
			if err := bad.Validate(); err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("Validate() = %v, want %q", err, tc.want)
			}
			if _, err := event.Encode(bad); err == nil {
				t.Fatal("Encode accepted malformed tools")
			}
		})
	}
	good := base
	good.Tools = []string{"exec", "read", "yield"}
	if err := good.Validate(); err != nil {
		t.Fatalf("Validate() of sorted unique tools = %v", err)
	}
}
