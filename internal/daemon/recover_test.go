//go:build unix

package daemon

import (
	"context"
	"encoding/json/jsontext"
	"encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/rpc"
)

type execModel struct {
	command  string
	mu       sync.Mutex
	requests []string
}

func (m *execModel) handler() http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("GET /models", func(w http.ResponseWriter, _ *http.Request) {
		_, _ = io.WriteString(w, `{"data":[{"id":"gpt-test"}]}`)
	})
	mux.HandleFunc("POST /responses", m.respond)
	return mux
}

func (m *execModel) respond(w http.ResponseWriter, r *http.Request) {
	body, err := io.ReadAll(r.Body)
	if err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	m.mu.Lock()
	m.requests = append(m.requests, string(body))
	m.mu.Unlock()
	w.Header().Set("Content-Type", "text/event-stream")
	if strings.Contains(string(body), "function_call_output") {
		_, _ = io.WriteString(w, sse("response.output_text.delta", `{"type":"response.output_text.delta","output_index":0,"delta":"finished"}`)+completedEvent())
		return
	}
	item := `{"type":"function_call","call_id":"call_exec","name":"exec","arguments":`
	args := strconv.Quote(`{"command":` + strconv.Quote(m.command) + `}`)
	_, _ = io.WriteString(w, sse("response.output_item.added", `{"type":"response.output_item.added","output_index":1,"item":`+item+`""}}`)+
		sse("response.output_item.done", `{"type":"response.output_item.done","output_index":1,"item":`+item+args+`}}`)+completedEvent())
}

func (m *execModel) bodies() []string {
	m.mu.Lock()
	defer m.mu.Unlock()
	return slices.Clone(m.requests)
}

func sse(kind, data string) string { return "event: " + kind + "\ndata: " + data + "\n\n" }

func completedEvent() string {
	return sse("response.completed", `{"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":20,"output_tokens":7}}}`)
}

type ignoreServer struct{}

func (ignoreServer) Notification(string, jsontext.Value) {}

func (ignoreServer) Request(context.Context, string, jsontext.Value) (any, error) {
	return nil, fmt.Errorf("test client answers no server requests")
}

type recovery struct {
	command   string
	output    string
	exact     bool
	ok        bool
	recovered string
}

func TestDaemonRecoversTurnAfterKill(t *testing.T) {
	recoverAfterKill(t, recovery{command: "sleep 3; echo ok", output: "ok", exact: true, ok: true, recovered: "replayed"}, func(*testing.T, string) {})
}

func recoverAfterKill(t *testing.T, want recovery, afterDaemonKill func(t *testing.T, runDir string)) {
	t.Helper()
	home, work := shortHome(t), t.TempDir()
	model := &execModel{command: want.command}
	server := httptest.NewServer(model.handler())
	t.Cleanup(server.Close)
	daemonEnv(t, home, server.URL)
	exe, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { killDaemon(t, home) })
	threadID := startExecTurn(t, home, exe, work)
	runDir := filepath.Join(home, "sessions", threadID, "runs", "call_exec")
	waitForPath(t, filepath.Join(runDir, "pid"))
	killDaemon(t, home)
	afterDaemonKill(t, runDir)
	conn, err := Ensure(t.Context(), home, exe)
	if err != nil {
		t.Fatalf("Ensure after kill: %v", err)
	}
	if closeErr := conn.Close(); closeErr != nil {
		t.Fatal(closeErr)
	}
	journal := waitForJournal(t, filepath.Join(home, "sessions", threadID, "journal.jsonl"), `"turn.completed"`)
	requireRecoveredOutput(t, journal, model.bodies(), want)
	runs, err := os.ReadDir(filepath.Join(home, "sessions", threadID, "runs"))
	if err != nil || len(runs) != 1 {
		t.Fatalf("exec runs = %v, %v; want exactly one", runs, err)
	}
}

func daemonEnv(t *testing.T, home, modelURL string) {
	t.Helper()
	t.Setenv("DALGONA_HOME", home)
	t.Setenv("OPENAI_BASE_URL", modelURL)
	t.Setenv("OPENAI_API_KEY", "test-key")
	t.Setenv(realDaemonEnv, "1")
	if err := os.WriteFile(filepath.Join(home, "config.toml"), []byte("model = \"openai/gpt-test\"\napproval = \"yolo\"\n"), 0o600); err != nil {
		t.Fatal(err)
	}
}

func startExecTurn(t *testing.T, home, exe, work string) string {
	t.Helper()
	conn, err := Ensure(t.Context(), home, exe)
	if err != nil {
		t.Fatal(err)
	}
	client, err := rpc.Dial(conn, ignoreServer{})
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = client.Close() }()
	ctx, cancel := context.WithTimeout(t.Context(), 30*time.Second)
	defer cancel()
	var initialized rpc.InitializeResult
	if err := client.Initialize(ctx, rpc.InitializeParams{ClientInfo: rpc.ClientInfo{Name: "daemon-test", Version: "0"}}, &initialized); err != nil {
		t.Fatalf("initialize: %v", err)
	}
	var started struct {
		Thread rpc.ThreadID `json:"thread"`
	}
	if err := client.ThreadStart(ctx, rpc.ThreadStartParams{Cwd: work}, &started); err != nil {
		t.Fatalf("thread/start: %v", err)
	}
	turn := rpc.TurnStartParams{ThreadID: started.Thread.ID, Input: []rpc.InputItem{{Type: "text", Text: "run it"}}}
	if err := client.TurnStart(ctx, turn, nil); err != nil {
		t.Fatalf("turn/start: %v", err)
	}
	return started.Thread.ID
}

func waitForPath(t *testing.T, path string) {
	t.Helper()
	deadline := time.Now().Add(20 * time.Second)
	for time.Now().Before(deadline) {
		if _, err := os.Stat(path); err == nil {
			return
		}
		time.Sleep(20 * time.Millisecond)
	}
	t.Fatalf("%s never appeared", path)
}

func waitForJournal(t *testing.T, path, marker string) string {
	t.Helper()
	deadline := time.Now().Add(30 * time.Second)
	for time.Now().Before(deadline) {
		data, err := os.ReadFile(path)
		if err == nil && strings.Contains(string(data), marker) {
			return string(data)
		}
		time.Sleep(50 * time.Millisecond)
	}
	t.Fatalf("journal never recorded %s", marker)
	return ""
}

type journalLine struct {
	Event struct {
		Type      string `json:"type"`
		CallID    string `json:"call_id"`
		OK        bool   `json:"ok"`
		Output    string `json:"output"`
		Recovered string `json:"recovered"`
	} `json:"ev"`
}

func requireRecoveredOutput(t *testing.T, journal string, requests []string, want recovery) {
	t.Helper()
	completions := toolCompletions(t, journal)
	if len(completions) != 1 {
		t.Fatalf("tool.completed events = %+v; want one", completions)
	}
	got := completions[0].Event
	if got.OK != want.ok || got.Recovered != want.recovered || !outputMatches(got.Output, want) {
		t.Fatalf("recovered exec completion = %+v; want %+v", got, want)
	}
	if len(requests) != 2 {
		t.Fatalf("model saw %d requests; want the call and the recovered follow-up", len(requests))
	}
	if output, found := callOutput(t, requests[1], "call_exec"); !found || !outputMatches(output, want) {
		t.Fatalf("follow-up function_call_output for call_exec = %q, %v; want %q", output, found, want.output)
	}
}

func outputMatches(output string, want recovery) bool {
	if want.exact {
		return strings.TrimSpace(output) == want.output
	}
	return strings.Contains(output, want.output)
}

func toolCompletions(t *testing.T, journal string) []journalLine {
	t.Helper()
	var completions []journalLine
	for line := range strings.Lines(journal) {
		var decoded journalLine
		if err := json.Unmarshal([]byte(line), &decoded); err != nil {
			t.Fatalf("journal line %q: %v", line, err)
		}
		if decoded.Event.Type == "tool.completed" {
			completions = append(completions, decoded)
		}
	}
	return completions
}

func callOutput(t *testing.T, request, callID string) (string, bool) {
	t.Helper()
	var body struct {
		Input []struct {
			Type   string `json:"type"`
			CallID string `json:"call_id"`
			Output string `json:"output"`
		} `json:"input"`
	}
	if err := json.Unmarshal([]byte(request), &body); err != nil {
		t.Fatalf("decode follow-up request: %v", err)
	}
	for _, item := range body.Input {
		if item.Type == "function_call_output" && item.CallID == callID {
			return item.Output, true
		}
	}
	return "", false
}

func killDaemon(t *testing.T, home string) {
	t.Helper()
	data, err := os.ReadFile(filepath.Join(home, "daemon.pid"))
	if err != nil {
		return
	}
	pid, err := strconv.Atoi(strings.TrimSpace(string(data)))
	if err != nil {
		t.Fatalf("daemon pid: %v", err)
	}
	if err := syscall.Kill(pid, syscall.SIGKILL); err != nil && !errors.Is(err, syscall.ESRCH) {
		t.Fatalf("kill daemon: %v", err)
	}
	if err := os.Remove(filepath.Join(home, "daemon.pid")); err != nil {
		t.Fatal(err)
	}
	waitForDaemonHelperExit(t, home)
}
