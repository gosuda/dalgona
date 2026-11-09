package runner

import (
	"strings"
	"testing"
)

func TestRunnerEnvironment_stripsHomeWhenSandboxed(t *testing.T) {
	base := []string{"PATH=/usr/bin", "DALGONA_HOME=/secret/home", "DAL_HOME=/secret/dal", "HOME=/root"}
	env := runnerEnvironment(base, "/tmp/work", SandboxWorkspaceWrite, []string{"/tmp/work"})
	for _, item := range env {
		if strings.HasPrefix(item, "DALGONA_HOME=") || strings.HasPrefix(item, "DAL_HOME=") {
			t.Fatalf("home leaked into sandboxed child environment: %q", item)
		}
	}
	found := false
	for _, item := range env {
		if item == "HOME=/root" {
			found = true
		}
	}
	if !found {
		t.Fatalf("HOME must stay in sandboxed child environment: %q", env)
	}
}

func TestRunnerEnvironment_keepsHomeWhenUnsandboxed(t *testing.T) {
	base := []string{"PATH=/usr/bin", "DALGONA_HOME=/secret/home"}
	env := runnerEnvironment(base, "/tmp/work", SandboxOff, nil)
	found := false
	for _, item := range env {
		if item == "DALGONA_HOME=/secret/home" {
			found = true
		}
	}
	if !found {
		t.Fatalf("off-mode child lost DALGONA_HOME: %q", env)
	}
}
