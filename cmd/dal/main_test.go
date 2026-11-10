package main

import (
	"bytes"
	"context"
	json "encoding/json/v2"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"
)

const asDal = "DAL_TEST_RUN_AS_DAL"

func TestMain(m *testing.M) {
	if os.Getenv(asDal) == "1" {
		os.Exit(run(context.Background(), os.Args[1:], os.Stdin, os.Stdout, os.Stderr))
	}
	os.Exit(m.Run())
}

type fakeOpenAI struct {
	server  *httptest.Server
	mu      sync.Mutex
	replies []string
	served  int
}

func newFakeOpenAI(t *testing.T, replies ...string) *fakeOpenAI {
	t.Helper()
	f := &fakeOpenAI{replies: replies}
	mux := http.NewServeMux()
	mux.HandleFunc("POST /responses", func(w http.ResponseWriter, r *http.Request) {
		if _, err := io.Copy(io.Discard, r.Body); err != nil {
			http.Error(w, err.Error(), http.StatusBadRequest)
			return
		}
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = io.WriteString(w, f.next())
	})
	mux.HandleFunc("GET /models", func(w http.ResponseWriter, _ *http.Request) {
		_, _ = io.WriteString(w, `{"data":[{"id":"gpt-test"}]}`)
	})
	f.server = httptest.NewServer(mux)
	t.Cleanup(f.server.Close)
	return f
}

func (f *fakeOpenAI) next() string {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.served++
	if len(f.replies) == 0 {
		return sseText("finished")
	}
	reply := f.replies[0]
	f.replies = f.replies[1:]
	return reply
}

func sseEvent(kind, data string) string { return "event: " + kind + "\ndata: " + data + "\n\n" }

func sseCompleted() string {
	return sseEvent("response.completed", `{"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":5,"output_tokens":3}}}`)
}

func sseText(text string) string {
	return sseEvent("response.output_text.delta", `{"type":"response.output_text.delta","output_index":0,"delta":`+strconv.Quote(text)+`}`) + sseCompleted()
}

func sseExec(id, command string) string {
	args := strconv.Quote(`{"command":` + strconv.Quote(command) + `}`)
	head := `{"type":"function_call","call_id":"` + id + `","name":"exec","arguments":`
	return sseEvent("response.output_item.added", `{"type":"response.output_item.added","output_index":0,"item":`+head+`""}}`) +
		sseEvent("response.output_item.done", `{"type":"response.output_item.done","output_index":0,"item":`+head+args+`}}`) +
		sseCompleted()
}

type dalEnv struct {
	t    *testing.T
	home string
	cwd  string
	api  *fakeOpenAI
}

func newDalEnv(t *testing.T, replies ...string) *dalEnv {
	t.Helper()
	return &dalEnv{t: t, home: t.TempDir(), cwd: t.TempDir(), api: newFakeOpenAI(t, replies...)}
}

func (e *dalEnv) command(ctx context.Context, args ...string) (*exec.Cmd, *bytes.Buffer, *bytes.Buffer) {
	exe, err := os.Executable()
	if err != nil {
		e.t.Fatal(err)
	}
	cmd := exec.CommandContext(ctx, exe, args...)
	cmd.Env = append(os.Environ(), asDal+"=1", "DAL_HOME="+e.home, "OPENAI_BASE_URL="+e.api.server.URL, "OPENAI_API_KEY=sk-test")
	var stdout, stderr bytes.Buffer
	cmd.Stdout, cmd.Stderr = &stdout, &stderr
	return cmd, &stdout, &stderr
}

type statusLine struct {
	Thread     string `json:"thread"`
	Status     string `json:"status"`
	StopReason string `json:"stop_reason"`
}

func (e *dalEnv) runJSON(args ...string) statusLine {
	e.t.Helper()
	ctx, cancel := context.WithTimeout(e.t.Context(), time.Minute)
	defer cancel()
	cmd, stdout, stderr := e.command(ctx, args...)
	if err := cmd.Run(); err != nil {
		e.t.Fatalf("dal %v: %v\nstdout: %s\nstderr: %s", args, err, stdout, stderr)
	}
	lines := strings.Split(strings.TrimSpace(stdout.String()), "\n")
	var line statusLine
	if err := json.Unmarshal([]byte(lines[len(lines)-1]), &line); err != nil {
		e.t.Fatalf("stdout %q: %v", stdout, err)
	}
	return line
}

func TestPrintModeEndToEnd(t *testing.T) {
	e := newDalEnv(t, sseText("hello there"))
	line := e.runJSON("-p", "say hi", "--json", "-m", "openai/gpt-test", "--approval", "yolo", "-C", e.cwd)
	if line.Thread == "" || line.Status != "completed" || line.StopReason != "stop" {
		t.Fatalf("status line %+v", line)
	}
	ctx, cancel := context.WithTimeout(t.Context(), time.Minute)
	defer cancel()
	cmd, stdout, stderr := e.command(ctx, "-p", "again", "--resume", line.Thread, "-C", e.cwd)
	if err := cmd.Run(); err != nil {
		t.Fatalf("resume: %v\n%s", err, stderr)
	}
	if strings.TrimSpace(stdout.String()) != "finished" {
		t.Fatalf("text mode stdout %q", stdout)
	}
	second := e.runJSON("-p", "another", "--json", "-m", "openai/gpt-test", "--approval", "yolo", "-C", e.cwd)
	if second.Thread == line.Thread {
		t.Fatalf("second -p reused thread %s", line.Thread)
	}
	latest := e.runJSON("-c", "-p", "", "--json", "-C", e.cwd)
	if latest.Thread != second.Thread || latest.Status != "completed" {
		t.Fatalf("-c status %+v, want newest thread %s completed", latest, second.Thread)
	}
}

func holdCommand() string {
	if runtime.GOOS == "windows" {
		return "echo run >> runs.txt & ping -n 4 127.0.0.1 >nul & echo ok"
	}
	return "echo run >> runs.txt; sleep 3; echo ok"
}

func TestKilledTurnResumesFromShim(t *testing.T) {
	e := newDalEnv(t, sseExec("call-sleep", holdCommand()))
	ctx, cancel := context.WithTimeout(t.Context(), time.Minute)
	defer cancel()
	cmd, _, stderr := e.command(ctx, "-p", "run it", "--json", "-m", "openai/gpt-test", "--approval", "yolo", "-C", e.cwd)
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	journal := waitForJournal(t, e.home, `"tool.started"`)
	waitForFile(t, filepath.Join(filepath.Dir(journal), "runs", "*", "pid"))
	if err := cmd.Process.Kill(); err != nil {
		t.Fatal(err)
	}
	if err := cmd.Wait(); err == nil {
		t.Fatalf("dal exited cleanly before the kill; stderr: %s", stderr)
	}
	thread := filepath.Base(filepath.Dir(journal))
	line := e.runJSON("--resume", thread, "-p", "", "--json", "-C", e.cwd)
	if line.Thread != thread || line.Status != "completed" {
		t.Fatalf("resumed status %+v", line)
	}
	checkRecovered(t, journal, filepath.Join(e.cwd, "runs.txt"))
}

func checkRecovered(t *testing.T, journal, marker string) {
	t.Helper()
	data, err := os.ReadFile(journal)
	if err != nil {
		t.Fatal(err)
	}
	if !completedWithOK(data) {
		t.Fatalf("journal has no tool.completed with output ok:\n%s", data)
	}
	runs, err := os.ReadFile(marker)
	if err != nil {
		t.Fatalf("command ran %q times (err %v); recovery must not rerun it", runs, err)
	}
	if fields := strings.Fields(string(runs)); len(fields) != 1 || fields[0] != "run" {
		t.Fatalf("command ran %q times (err %v); recovery must not rerun it", runs, err)
	}
}

func completedWithOK(journal []byte) bool {
	for line := range strings.SplitSeq(string(journal), "\n") {
		if strings.Contains(line, `"tool.completed"`) && strings.Contains(line, `"output":"ok`) {
			return true
		}
	}
	return false
}

func waitForJournal(t *testing.T, home, marker string) string {
	t.Helper()
	deadline := time.Now().Add(30 * time.Second)
	for time.Now().Before(deadline) {
		matches, err := filepath.Glob(filepath.Join(home, "sessions", "*", "journal.jsonl"))
		if err != nil {
			t.Fatal(err)
		}
		for _, path := range matches {
			data, err := os.ReadFile(path)
			if err == nil && bytes.Contains(data, []byte(marker)) {
				return path
			}
		}
		time.Sleep(20 * time.Millisecond)
	}
	t.Fatalf("no journal with %s", marker)
	return ""
}

func waitForFile(t *testing.T, pattern string) {
	t.Helper()
	deadline := time.Now().Add(30 * time.Second)
	for time.Now().Before(deadline) {
		matches, err := filepath.Glob(pattern)
		if err != nil {
			t.Fatal(err)
		}
		if len(matches) > 0 {
			return
		}
		time.Sleep(20 * time.Millisecond)
	}
	t.Fatalf("no file matches %s", pattern)
}

func TestUsageErrors(t *testing.T) {
	var stdout, stderr bytes.Buffer
	if code := run(t.Context(), []string{"--approval", "sometimes"}, strings.NewReader(""), &stdout, &stderr); code != 2 {
		t.Fatalf("exit %d, stderr %s", code, stderr.String())
	}
	if !strings.Contains(stderr.String(), "--approval must be auto, ask, or yolo") {
		t.Fatalf("stderr %q", stderr.String())
	}
}
