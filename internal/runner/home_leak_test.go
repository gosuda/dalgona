package runner

import (
	"strings"
	"testing"
)

func TestRunnerEnvironment_stripsHomeWhenSandboxed(t *testing.T) {
	base := []string{"PATH=/usr/bin", "DALGONA_HOME=/secret/home", "DAL_HOME=/secret/dal", "HOME=/root"}
	env := runnerEnvironment(base, "/tmp/work", Sandbox{Mode: SandboxWorkspaceWrite, WritableRoots: []string{"/tmp/work"}})
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
	env := runnerEnvironment(base, "/tmp/work", Sandbox{Mode: SandboxOff})
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

func TestStripDeniedEnvironment_hidesSocketFromCommand(t *testing.T) {
	base := []string{"PATH=/usr/bin", "DALGONA_HOME=/secret/home", "HOME=/root"}
	env := runnerEnvironment(base, "/tmp/work", Sandbox{Mode: SandboxWorkspaceWrite, WritableRoots: []string{"/tmp/work"}, DeniedSockets: []string{"/secret/home/dalgona.sock"}})
	for _, item := range stripDeniedEnvironment(env) {
		if strings.HasPrefix(item, "DALGONA_SANDBOX_DENIED=") {
			t.Fatalf("denied socket variable leaked into command environment: %q", item)
		}
		if strings.Contains(item, "dalgona.sock") {
			t.Fatalf("socket path leaked into command environment: %q", item)
		}
	}
}
