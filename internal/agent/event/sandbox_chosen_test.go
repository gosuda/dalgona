package event_test

import (
	"reflect"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
)

func TestSandboxChosenRoundTrip(t *testing.T) {
	t.Parallel()
	for _, choice := range []string{event.SandboxChoiceAccept, event.SandboxChoiceDangerous} {
		original := event.SandboxChosen{RequestID: "req-1", Choice: choice, Cause: "Landlock is not available on this kernel"}
		encoded, err := event.Encode(original)
		if err != nil {
			t.Fatalf("encode %s: %v", choice, err)
		}
		if !strings.Contains(string(encoded), `"sandbox.chosen"`) {
			t.Fatalf("encoded %s lacks the sandbox.chosen type", encoded)
		}
		decoded, err := event.Decode(encoded)
		if err != nil {
			t.Fatalf("decode %s: %v", choice, err)
		}
		if !reflect.DeepEqual(decoded, event.Event(original)) {
			t.Fatalf("round trip for %s = %#v, want %#v", choice, decoded, original)
		}
	}
}

func TestSandboxChosenValidation(t *testing.T) {
	t.Parallel()
	valid := event.SandboxChosen{RequestID: "req-1", Choice: event.SandboxChoiceAccept, Cause: "cause"}
	tests := []struct {
		name    string
		mutate  func(*event.SandboxChosen)
		wantErr bool
	}{
		{name: "accept is valid", mutate: func(*event.SandboxChosen) {}},
		{name: "dangerously accept is valid", mutate: func(e *event.SandboxChosen) { e.Choice = event.SandboxChoiceDangerous }},
		{name: "empty request id", mutate: func(e *event.SandboxChosen) { e.RequestID = "" }, wantErr: true},
		{name: "empty cause", mutate: func(e *event.SandboxChosen) { e.Cause = "" }, wantErr: true},
		{name: "unknown choice", mutate: func(e *event.SandboxChosen) { e.Choice = "maybe" }, wantErr: true},
		{name: "cause over one KiB", mutate: func(e *event.SandboxChosen) { e.Cause = strings.Repeat("x", 1<<10+1) }, wantErr: true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			value := valid
			tt.mutate(&value)
			if err := value.Validate(); (err != nil) != tt.wantErr {
				t.Fatalf("Validate() = %v, want error %t", err, tt.wantErr)
			}
		})
	}
}
