package runner

import (
	"slices"
	"strings"
	"testing"
)

func TestRunnerEnvironment_rootsRoundTripWithSeparator(t *testing.T) {
	env := runnerEnvironment(nil, "/tmp/x:/etc", SandboxWorkspaceWrite, []string{"/tmp/y:z", "/tmp/x:/etc"})
	var raw string
	for _, item := range env {
		if value, ok := strings.CutPrefix(item, "DALGONA_SANDBOX_ROOTS="); ok {
			raw = value
		}
	}
	if raw == "" {
		t.Fatal("DALGONA_SANDBOX_ROOTS not set")
	}
	got := splitRoots(raw)
	want := []string{"/tmp/y:z", "/tmp/x:/etc"}
	if !slices.Equal(got, want) {
		t.Fatalf("splitRoots = %v, want %v", got, want)
	}
}

func TestRunnerEnvironment_emptyRoots(t *testing.T) {
	env := runnerEnvironment(nil, "", SandboxOff, nil)
	var raw string
	for _, item := range env {
		if value, ok := strings.CutPrefix(item, "DALGONA_SANDBOX_ROOTS="); ok {
			raw = value
		}
	}
	if got := splitRoots(raw); len(got) != 0 {
		t.Fatalf("splitRoots = %v, want empty", got)
	}
}
