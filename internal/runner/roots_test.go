package runner

import (
	"slices"
	"strings"
	"testing"
)

func TestRunnerEnvironment_rootsRoundTripWithSeparator(t *testing.T) {
	env := runnerEnvironment(nil, "/tmp/x:/etc", Sandbox{Mode: SandboxWorkspaceWrite, WritableRoots: []string{"/tmp/y:z", "/tmp/x:/etc"}})
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
	env := runnerEnvironment(nil, "", Sandbox{Mode: SandboxOff})
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

func TestRunnerEnvironment_stripsBoxToken(t *testing.T) {
	base := []string{"PATH=/usr/bin", boxEnvironmentKey + "=secret-token-value", "HOME=/root"}
	env := runnerEnvironment(base, "/tmp/work", Sandbox{Mode: SandboxWorkspaceWrite, WritableRoots: []string{"/tmp/work"}})
	for _, item := range env {
		if strings.HasPrefix(item, boxEnvironmentKey+"=") {
			t.Fatalf("box token leaked into child environment: %q", item)
		}
	}
}

func TestRunnerEnvironment_deniedSocketsRoundTrip(t *testing.T) {
	env := runnerEnvironment(nil, "/tmp/work", Sandbox{Mode: SandboxWorkspaceWrite, DeniedSockets: []string{"/tmp/home/dalgona.sock"}})
	var raw string
	for _, item := range env {
		if value, ok := strings.CutPrefix(item, "DALGONA_SANDBOX_DENIED="); ok {
			raw = value
		}
	}
	if raw == "" {
		t.Fatal("DALGONA_SANDBOX_DENIED not set")
	}
	got := splitRoots(raw)
	want := []string{"/tmp/home/dalgona.sock"}
	if !slices.Equal(got, want) {
		t.Fatalf("splitRoots = %v, want %v", got, want)
	}
}
