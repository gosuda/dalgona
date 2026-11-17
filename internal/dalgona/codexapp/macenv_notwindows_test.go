//go:build !windows

package codexapp

import (
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
)

func TestMacEnvironmentUsesInjectedRunner(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	var calls [][]string
	runner := func(name string, args ...string) ([]byte, error) {
		calls = append(calls, append([]string{name}, args...))
		if len(args) > 0 && args[0] == "getenv" {
			return []byte("/old/dalgona\n"), nil
		}
		return nil, nil
	}
	env := Env{
		Home:           home,
		DalgonaHome:    filepath.Join(home, ".dalgona"),
		GOOS:           "darwin",
		VSCodeSettings: filepath.Join(home, "settings.json"),
		CommandRunner:  runner,
	}
	exe := "/Applications/dalgona"
	if err := Install(env, exe); err != nil {
		t.Fatalf("Install: %v", err)
	}
	agentPath := launchAgentPath(env)
	agent, err := os.ReadFile(agentPath)
	if err != nil {
		t.Fatalf("read launch agent: %v", err)
	}
	for _, part := range []string{"/bin/launchctl", exe, "<key>RunAtLoad</key><true/>"} {
		if !strings.Contains(string(agent), part) {
			t.Fatalf("launch agent does not contain %q: %s", part, agent)
		}
	}
	if err := Uninstall(env); err != nil {
		t.Fatalf("Uninstall: %v", err)
	}
	wantCalls := [][]string{
		{"launchctl", "getenv", "CODEX_CLI_PATH"},
		{"launchctl", "setenv", "CODEX_CLI_PATH", exe},
		{"launchctl", "setenv", "CODEX_CLI_PATH", "/old/dalgona"},
	}
	if !reflect.DeepEqual(calls, wantCalls) {
		t.Fatalf("command argv = %#v, want %#v", calls, wantCalls)
	}
}
