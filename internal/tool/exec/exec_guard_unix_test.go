//go:build !windows

package exec

import (
	"encoding/json/jsontext"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/tool"
)

func TestExecRefusesProtectedKillBeforeRunning(t *testing.T) {
	env, closeRunner := testEnv(t)
	defer closeRunner()
	result, err := New().Run(t.Context(), tool.Call{ID: "refused-kill", Args: jsontext.Value(`{"command":"touch marker; kill -0 $PPID"}`)}, env)
	if err != nil {
		t.Fatal(err)
	}
	if result.OK || result.ExitCode != nil || !strings.HasPrefix(string(result.Output), "refused: process-kill guard") {
		t.Fatalf("result ok=%v exit=%v output=%q, want the guard refusal", result.OK, result.ExitCode, result.Output)
	}
	if _, statErr := os.Stat(filepath.Join(env.Cwd, "marker")); !errors.Is(statErr, os.ErrNotExist) {
		t.Fatalf("the refused command ran: %v", statErr)
	}
}

func TestExecRunsEchoedKillText(t *testing.T) {
	env, closeRunner := testEnv(t)
	defer closeRunner()
	result, err := New().Run(t.Context(), tool.Call{ID: "echoed-kill", Args: jsontext.Value(`{"command":"echo kill -0 -1"}`)}, env)
	if err != nil {
		t.Fatal(err)
	}
	if !result.OK || strings.TrimSpace(string(result.Output)) != "kill -0 -1" {
		t.Fatalf("result ok=%v output=%q, want the echoed text", result.OK, result.Output)
	}
}
