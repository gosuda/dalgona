package runner

import (
	"os/exec"
	"slices"
	"strings"
	"testing"
)

func childEnvironment(t *testing.T, env []string) []string {
	t.Helper()
	cmd := exec.Command("env")
	cmd.Env = env
	output, err := cmd.Output()
	if err != nil {
		t.Fatal(err)
	}
	return strings.Split(strings.TrimSpace(string(output)), "\n")
}

func TestRunnerEnvironment_stripsHomeWhenSandboxed(t *testing.T) {
	base := []string{"PATH=/usr/bin", "DALGONA_HOME=/secret/home", "DAL_HOME=/secret/dal", "HOME=/root"}
	env := runnerEnvironment(base, "/tmp/work", Sandbox{Mode: SandboxWorkspaceWrite, WritableRoots: []string{"/tmp/work"}})
	child := childEnvironment(t, env)
	for _, item := range child {
		if strings.HasPrefix(item, "DALGONA_HOME=") || strings.HasPrefix(item, "DAL_HOME=") {
			t.Fatalf("home leaked into sandboxed child: %q", item)
		}
	}
	if !slices.Contains(child, "HOME=/root") {
		t.Fatalf("HOME must stay in sandboxed child environment: %q", child)
	}
}

func TestRunnerEnvironment_keepsHomeWhenUnsandboxed(t *testing.T) {
	base := []string{"PATH=/usr/bin", "DALGONA_HOME=/secret/home", "DAL_HOME=/secret/dal"}
	child := childEnvironment(t, runnerEnvironment(base, "/tmp/work", Sandbox{Mode: SandboxOff}))
	for _, want := range []string{"DALGONA_HOME=/secret/home", "DAL_HOME=/secret/dal"} {
		if !slices.Contains(child, want) {
			t.Fatalf("off-mode child lost %q: %q", want, child)
		}
	}
}

func TestStripDeniedEnvironment_hidesSocketFromCommand(t *testing.T) {
	base := []string{"PATH=/usr/bin", "DALGONA_HOME=/secret/home", "HOME=/root"}
	env := runnerEnvironment(base, "/tmp/work", Sandbox{Mode: SandboxWorkspaceWrite, WritableRoots: []string{"/tmp/work"}, DeniedSockets: []string{"/secret/home/dalgona.sock"}})
	child := childEnvironment(t, stripDeniedEnvironment(env))
	for _, want := range []string{"PATH=/usr/bin", "HOME=/root"} {
		if !slices.Contains(child, want) {
			t.Fatalf("child lost %q: %q", want, child)
		}
	}
	for _, item := range child {
		if strings.HasPrefix(item, "DALGONA_SANDBOX_DENIED=") || strings.Contains(item, "dalgona.sock") {
			t.Fatalf("denied socket leaked into child: %q", item)
		}
	}
}
