package herdr

import (
	"bytes"
	"context"
	"log/slog"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
)

func TestReporterCommands(t *testing.T) {
	requireUnixShell(t)
	dir := t.TempDir()
	record := filepath.Join(dir, "argv")
	binary := filepath.Join(dir, "herdr")
	script := "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$HERDR_RECORD\"\nif [ \"$1\" = pane ] && [ \"$2\" = split ]; then\n  printf '%s\\n' '{\"result\":{\"pane\":{\"pane_id\":\"child-pane\"}}}'\nfi\n"
	if err := os.WriteFile(binary, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("HERDR_RECORD", record)
	r := FromEnv(reporterEnv(binary))
	if r == nil {
		t.Fatal("expected active reporter")
	}
	r.Report(context.Background(), StateWorking, 2, "thread-1")
	r.Report(context.Background(), StateBlocked, 9, "thread-1")
	r.SubagentPane(context.Background(), "child-1", "/workspace")
	r.Release(context.Background())
	assertArgv(t, record)
}

func reporterEnv(binary string) func(string) string {
	return func(key string) string {
		switch key {
		case "HERDR_ENV":
			return "1"
		case "HERDR_PANE_ID":
			return "root-pane"
		case "HERDR_BIN_PATH":
			return binary
		default:
			return ""
		}
	}
}

func assertArgv(t *testing.T, record string) {
	t.Helper()
	data, err := os.ReadFile(record)
	if err != nil {
		t.Fatal(err)
	}
	want := strings.Join([]string{
		"pane report-agent root-pane --source dalgona --agent dalgona --state working --seq 2 --agent-session-id thread-1 -- dalgona --resume thread-1",
		"pane report-agent root-pane --source dalgona --agent dalgona --state blocked --seq 9 --agent-session-id thread-1 -- dalgona --resume thread-1",
		"pane split --current --direction right --cwd /workspace --no-focus",
		"pane run child-pane dalgona --resume child-1",
		"pane release-agent root-pane --source dalgona --agent dalgona",
	}, "\n") + "\n"
	if string(data) != want {
		t.Fatalf("argv = %q, want %q", data, want)
	}
}

func requireUnixShell(t *testing.T) {
	t.Helper()
	if runtime.GOOS == "windows" {
		t.Skip("shell script test")
	}
}

func TestFromEnvInactive(t *testing.T) {
	if got := FromEnv(func(string) string { return "" }); got != nil {
		t.Fatal("inactive environment returned a reporter")
	}
	if got := FromEnv(nil); got != nil {
		t.Fatal("nil environment returned a reporter")
	}
}

func TestReporterLogsFailureOnce(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("shell script test")
	}
	dir := t.TempDir()
	binary := filepath.Join(dir, "herdr")
	if err := os.WriteFile(binary, []byte("#!/bin/sh\nexit 1\n"), 0o700); err != nil {
		t.Fatal(err)
	}
	var logs bytes.Buffer
	previous := slog.Default()
	slog.SetDefault(slog.New(slog.NewTextHandler(&logs, nil)))
	t.Cleanup(func() { slog.SetDefault(previous) })
	r := &Reporter{binary: binary, paneID: "root-pane"}
	r.Report(context.Background(), StateIdle, 1, "thread-1")
	r.Release(context.Background())
	if got := strings.Count(logs.String(), "herdr: command failed"); got != 1 {
		t.Fatalf("failure log count = %d, want 1", got)
	}
}

func TestSplitPaneID(t *testing.T) {
	got, err := splitPaneID([]byte(`{"result":{"pane":{"pane_id":"p-1"}}}`))
	if err != nil {
		t.Fatal(err)
	}
	if got != "p-1" {
		t.Fatalf("pane id = %q, want p-1", got)
	}
	if _, err := splitPaneID([]byte(`{"result":{}}`)); err == nil {
		t.Fatal("missing pane did not fail")
	}
}
