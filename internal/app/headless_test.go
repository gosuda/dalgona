package app

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"strings"
	"sync"
	"testing"
	"testing/synctest"
	"time"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/rpc"
)

type scriptedNote struct {
	method string
	params string
}

type scriptedDaemon struct {
	t           *testing.T
	startNotes  []scriptedNote
	resumeNotes []scriptedNote
	resumeReply string
	mu          sync.Mutex
	dials       int
	methods     []string
	inputs      []string
}

func newScriptedDaemon(t *testing.T, startNotes ...scriptedNote) *scriptedDaemon {
	t.Helper()
	return &scriptedDaemon{t: t, startNotes: startNotes}
}

func (d *scriptedDaemon) dial(ctx context.Context) (io.ReadWriteCloser, error) {
	d.mu.Lock()
	d.dials++
	d.mu.Unlock()
	client, server := net.Pipe()
	var conn *rpc.Conn
	conn = rpc.NewConn(ctx, server, func(_ context.Context, method string, params jsontext.Value) (jsontext.Value, error) {
		return d.handle(conn, method, params)
	})
	d.t.Cleanup(func() { _ = conn.Close() })
	return client, nil
}

func (d *scriptedDaemon) handle(conn *rpc.Conn, method string, params jsontext.Value) (jsontext.Value, error) {
	d.mu.Lock()
	d.methods = append(d.methods, method)
	d.mu.Unlock()
	switch method {
	case "initialize":
		return jsontext.Value(`{}`), nil
	case "thread/start":
		return jsontext.Value(`{"thread":{"id":"th-1"}}`), nil
	case "thread/resume":
		return jsontext.Value(d.resumeReply), d.send(conn, d.resumeNotes)
	case "turn/start":
		return d.startTurn(conn, params)
	}
	return nil, fmt.Errorf("unexpected method %s", method)
}

func (d *scriptedDaemon) startTurn(conn *rpc.Conn, params jsontext.Value) (jsontext.Value, error) {
	var start rpc.TurnStartParams
	if err := json.Unmarshal(params, &start); err != nil {
		return nil, fmt.Errorf("decode turn/start: %w", err)
	}
	d.mu.Lock()
	for _, input := range start.Input {
		d.inputs = append(d.inputs, input.Text)
	}
	d.mu.Unlock()
	return jsontext.Value(`{"turn":{"id":"u1","items":[],"status":"inProgress"}}`), d.send(conn, d.startNotes)
}

func (d *scriptedDaemon) send(conn *rpc.Conn, notes []scriptedNote) error {
	for _, note := range notes {
		if err := conn.Notify(note.method, jsontext.Value(note.params)); err != nil {
			return fmt.Errorf("notify %s: %w", note.method, err)
		}
	}
	return nil
}

func (d *scriptedDaemon) dialCount() int {
	d.mu.Lock()
	defer d.mu.Unlock()
	return d.dials
}

func (d *scriptedDaemon) submitted() []string {
	d.mu.Lock()
	defer d.mu.Unlock()
	return slices.Clone(d.inputs)
}

func itemNote(thread, turn, item string) scriptedNote {
	return scriptedNote{method: "item/completed", params: fmt.Sprintf(`{"threadId":%q,"turnId":%q,"completedAtMs":1,"item":%s}`, thread, turn, item)}
}

func turnNote(thread, turn, status string) scriptedNote {
	return scriptedNote{method: "turn/completed", params: fmt.Sprintf(`{"threadId":%q,"turn":{"id":%q,"items":[],"status":%q}}`, thread, turn, status)}
}

func runHeadless(t *testing.T, daemon *scriptedDaemon, stdin io.Reader, args ...string) (int, string, string) {
	t.Helper()
	var stdout, stderr bytes.Buffer
	env := CLIEnv{Product: config.ProductDal, Dial: daemon.dial, Stdin: stdin, Stdout: &stdout, Stderr: &stderr}
	code := RunCLI(t.Context(), env, args)
	return code, stdout.String(), stderr.String()
}

type itemRecord struct {
	SchemaVersion int            `json:"schemaVersion"`
	Type          string         `json:"type"`
	Thread        string         `json:"thread"`
	Turn          string         `json:"turn"`
	Item          jsontext.Value `json:"item"`
}

type resultRecord struct {
	SchemaVersion int    `json:"schemaVersion"`
	Type          string `json:"type"`
	Thread        string `json:"thread"`
	Status        string `json:"status"`
	StopReason    string `json:"stop_reason"`
}

func jsonlLines(t *testing.T, stdout string) []string {
	t.Helper()
	if stdout == "" {
		return nil
	}
	if !strings.HasSuffix(stdout, "\n") {
		t.Fatalf("stdout %q does not end with a newline", stdout)
	}
	return strings.Split(strings.TrimSuffix(stdout, "\n"), "\n")
}

func TestHeadlessJSONLOneLinePerItem(t *testing.T) {
	items := []string{
		`{"type":"userMessage","id":"i1","content":[{"type":"text","text":"review"}]}`,
		`{"type":"commandExecution","id":"c1","command":"ls","cwd":"/w","source":"agent","status":"completed","aggregatedOutput":"a.txt\n","exitCode":0}`,
		`{"type":"agentMessage","id":"i3","text":"all done"}`,
	}
	notes := []scriptedNote{
		itemNote("th-1", "u1", items[0]),
		itemNote("th-2", "u2", `{"type":"agentMessage","id":"x1","text":"not ours"}`),
		itemNote("th-1", "u1", items[1]),
		itemNote("th-1", "u1", items[2]),
		turnNote("th-1", "u1", "completed"),
	}
	code, stdout, stderr := runHeadless(t, newScriptedDaemon(t, notes...), nil, "-p", "review", "--json")
	if code != 0 || stderr != "" {
		t.Fatalf("exit %d, stderr %q", code, stderr)
	}
	lines := jsonlLines(t, stdout)
	if len(lines) != len(items)+1 {
		t.Fatalf("stdout holds %d lines, want %d items and one result:\n%s", len(lines), len(items), stdout)
	}
	checkItemLines(t, lines[:len(items)], items)
	checkResultLine(t, lines[len(items)])
}

func checkItemLines(t *testing.T, lines []string, items []string) {
	t.Helper()
	for i, want := range items {
		var record itemRecord
		if err := json.Unmarshal([]byte(lines[i]), &record); err != nil {
			t.Fatalf("line %d %q: %v", i, lines[i], err)
		}
		if record.SchemaVersion != 1 || record.Type != "item" || record.Thread != "th-1" || record.Turn != "u1" || string(record.Item) != want {
			t.Fatalf("line %d = %+v item %s, want item %s of th-1 u1", i, record, record.Item, want)
		}
	}
}

func checkResultLine(t *testing.T, line string) {
	t.Helper()
	var result resultRecord
	if err := json.Unmarshal([]byte(line), &result); err != nil {
		t.Fatalf("result line %q: %v", line, err)
	}
	if want := (resultRecord{SchemaVersion: 1, Type: "result", Thread: "th-1", Status: "completed", StopReason: "stop"}); result != want {
		t.Fatalf("result line %+v, want %+v", result, want)
	}
}

func TestHeadlessWithoutJSONPrintsOnlyAgentText(t *testing.T) {
	notes := []scriptedNote{
		itemNote("th-1", "u1", `{"type":"userMessage","id":"i1","content":[{"type":"text","text":"review"}]}`),
		itemNote("th-1", "u1", `{"type":"agentMessage","id":"i3","text":"all done"}`),
		turnNote("th-1", "u1", "completed"),
	}
	code, stdout, stderr := runHeadless(t, newScriptedDaemon(t, notes...), nil, "-p", "review")
	if code != 0 || stdout != "all done\n" || stderr != "" {
		t.Fatalf("exit %d, stdout %q, stderr %q, want exit 0, stdout %q and no stderr", code, stdout, stderr, "all done\n")
	}
}

type failingWriter struct {
	mu       sync.Mutex
	err      error
	attempts int
}

func (w *failingWriter) Write([]byte) (int, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.attempts++
	return 0, w.err
}

func (w *failingWriter) tries() int {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.attempts
}

func TestHeadlessJSONLWriteFailureStopsOutput(t *testing.T) {
	message := `{"type":"agentMessage","id":"i3","text":"all done"}`
	notes := []scriptedNote{
		itemNote("th-1", "u1", message),
		itemNote("th-1", "u1", message),
		turnNote("th-1", "u1", "completed"),
	}
	sink := &failingWriter{err: errors.New("disk full")}
	var stderr bytes.Buffer
	env := CLIEnv{Product: config.ProductDal, Dial: newScriptedDaemon(t, notes...).dial, Stdout: sink, Stderr: &stderr}
	code := RunCLI(t.Context(), env, []string{"-p", "review", "--json"})
	if want := "dal: write jsonl: disk full\n"; code != 1 || stderr.String() != want || sink.tries() != 1 {
		t.Fatalf("exit %d, stderr %q, %d write attempts, want exit 1, stderr %q and one attempt", code, stderr.String(), sink.tries(), want)
	}
	code, stdout, stderr2 := runHeadless(t, newScriptedDaemon(t, notes...), nil, "-p", "review", "--json")
	if lines := jsonlLines(t, stdout); code != 0 || stderr2 != "" || len(lines) != 3 {
		t.Fatalf("healthy writer: exit %d, stderr %q, %d lines, want exit 0 and 3 lines", code, stderr2, len(lines))
	}
}

func TestHeadlessJSONLWriteFailureStopsSubmission(t *testing.T) {
	activeItem := itemNote("th-9", "u0", `{"type":"agentMessage","id":"i0","text":"earlier"}`)
	reply := `{"thread":{"id":"th-9","turns":[{"id":"u0","items":[],"status":"inProgress"}]}}`
	build := func() *scriptedDaemon {
		daemon := newScriptedDaemon(t, turnNote("th-9", "u1", "completed"))
		daemon.resumeReply = reply
		daemon.resumeNotes = []scriptedNote{activeItem, turnNote("th-9", "u0", "completed")}
		return daemon
	}
	failed := build()
	sink := &failingWriter{err: errors.New("disk full")}
	var stderr bytes.Buffer
	env := CLIEnv{Product: config.ProductDal, Dial: failed.dial, Stdout: sink, Stderr: &stderr}
	code := RunCLI(t.Context(), env, []string{"--resume", "th-9", "-p", "next", "--json"})
	if want := "dal: write jsonl: disk full\n"; code != 1 || stderr.String() != want || len(failed.submitted()) != 0 {
		t.Fatalf("exit %d, stderr %q, submitted %q, want exit 1, stderr %q and no submitted prompt", code, stderr.String(), failed.submitted(), want)
	}
	healthy := build()
	code, _, stderr2 := runHeadless(t, healthy, nil, "--resume", "th-9", "-p", "next", "--json")
	if want := []string{"next"}; code != 0 || stderr2 != "" || !slices.Equal(healthy.submitted(), want) {
		t.Fatalf("healthy writer: exit %d, stderr %q, submitted %q, want exit 0 and %q", code, stderr2, healthy.submitted(), want)
	}
}

func completedTurn() scriptedNote { return turnNote("th-1", "u1", "completed") }

func stdinFile(t *testing.T, content string) *os.File {
	t.Helper()
	path := filepath.Join(t.TempDir(), "stdin.txt")
	if err := os.WriteFile(path, []byte(content), 0o600); err != nil {
		t.Fatal(err)
	}
	file, err := os.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = file.Close() })
	return file
}

func TestPipedStdinPrecedesPrompt(t *testing.T) {
	cases := []struct {
		name  string
		stdin io.Reader
		want  string
	}{
		{"pipe", strings.NewReader("diff body\n"), "diff body\n\nreview"},
		{"regular file", stdinFile(t, "from file\n"), "from file\n\nreview"},
		{"carriage returns", strings.NewReader("a\r\n\r\n"), "a\n\nreview"},
		{"inner newlines", strings.NewReader("l1\nl2\n"), "l1\nl2\n\nreview"},
		{"invalid utf-8", strings.NewReader("bad\xff\n"), "bad\uFFFD\n\nreview"},
		{"empty stdin", strings.NewReader(""), "review"},
		{"newlines only", strings.NewReader("\n\n"), "review"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			daemon := newScriptedDaemon(t, completedTurn())
			code, _, stderr := runHeadless(t, daemon, tc.stdin, "-p", "review")
			if got := daemon.submitted(); code != 0 || !slices.Equal(got, []string{tc.want}) {
				t.Fatalf("exit %d, stderr %q, submitted %q, want %q", code, stderr, got, tc.want)
			}
		})
	}
}

func TestTerminalStdinLeavesPromptAlone(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("no character device with data on this platform")
	}
	terminal, err := os.Open("/dev/zero")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = terminal.Close() })
	daemon := newScriptedDaemon(t, completedTurn())
	code, _, stderr := runHeadless(t, daemon, terminal, "-p", "review")
	if got := daemon.submitted(); code != 0 || !slices.Equal(got, []string{"review"}) {
		t.Fatalf("exit %d, stderr %q, submitted %q, want only the prompt", code, stderr, got)
	}
}

func TestPipedStdinAloneIsThePromptForAnEmptyOne(t *testing.T) {
	daemon := newScriptedDaemon(t, completedTurn())
	code, _, stderr := runHeadless(t, daemon, strings.NewReader("only stdin\n"), "-p", "")
	if got := daemon.submitted(); code != 0 || !slices.Equal(got, []string{"only stdin"}) {
		t.Fatalf("exit %d, stderr %q, submitted %q, want only the stdin text", code, stderr, got)
	}
}

func TestHeadlessNeedsPromptOrInput(t *testing.T) {
	bare := newScriptedDaemon(t)
	code, stdout, stderr := runHeadless(t, bare, strings.NewReader(""), "-p", "")
	want := "dal: -p needs a prompt or piped input unless --resume or -c selects a thread\n"
	if code != 2 || stdout != "" || stderr != want || bare.dialCount() != 0 {
		t.Fatalf("exit %d, stdout %q, stderr %q, %d dials, want exit 2, stderr %q and no dial", code, stdout, stderr, bare.dialCount(), want)
	}
	resumed := newScriptedDaemon(t)
	resumed.resumeReply = `{"thread":{"id":"th-9","turns":[{"id":"u0","items":[],"status":"completed"}]}}`
	code, _, stderr = runHeadless(t, resumed, strings.NewReader(""), "--resume", "th-9", "-p", "")
	if code != 0 || stderr != "" || len(resumed.submitted()) != 0 {
		t.Fatalf("resume without prompt: exit %d, stderr %q, submitted %q, want exit 0 and nothing submitted", code, stderr, resumed.submitted())
	}
}

func TestPipedStdinWaitsForEOF(t *testing.T) {
	daemon := newScriptedDaemon(t, completedTurn())
	reader, writer := io.Pipe()
	t.Cleanup(func() { _ = writer.Close() })
	var stderr bytes.Buffer
	env := CLIEnv{Product: config.ProductDal, Dial: daemon.dial, Stdin: reader, Stdout: io.Discard, Stderr: &stderr}
	done := make(chan int, 1)
	go func() { done <- RunCLI(t.Context(), env, []string{"-p", "review"}) }()
	for _, part := range []string{"part one\n", "part two\n"} {
		if _, err := io.WriteString(writer, part); err != nil {
			t.Fatal(err)
		}
		if dials := daemon.dialCount(); dials != 0 {
			t.Fatalf("%d dials before stdin reached EOF, want none", dials)
		}
	}
	if err := writer.Close(); err != nil {
		t.Fatal(err)
	}
	select {
	case code := <-done:
		if want := []string{"part one\npart two\n\nreview"}; code != 0 || !slices.Equal(daemon.submitted(), want) {
			t.Fatalf("exit %d, stderr %q, submitted %q, want exactly %q", code, stderr.String(), daemon.submitted(), want)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("the run did not finish after stdin closed")
	}
}

func TestPipedStdinTooLarge(t *testing.T) {
	refused := newScriptedDaemon(t, completedTurn())
	code, stdout, stderr := runHeadless(t, refused, strings.NewReader(strings.Repeat("x", 1048577)), "-p", "review")
	want := "dal: piped input is larger than 1048576 bytes; write it to a file and ask the agent to read it\n"
	if code != 1 || stdout != "" || stderr != want || refused.dialCount() != 0 || len(refused.submitted()) != 0 {
		t.Fatalf("exit %d, stdout %q, stderr %q, %d dials, want exit 1, stderr %q and no request", code, stdout, stderr, refused.dialCount(), want)
	}
	accepted := newScriptedDaemon(t, completedTurn())
	code, _, stderr = runHeadless(t, accepted, strings.NewReader(strings.Repeat("x", 1048576)), "-p", "review")
	got := accepted.submitted()
	if code != 0 || len(got) != 1 || len(got[0]) != 1048576+len("\n\nreview") {
		t.Fatalf("limit-sized input: exit %d, stderr %q, %d prompts, want exit 0 and one prompt of 1048576 bytes plus the request", code, stderr, len(got))
	}
}

type shortJSONLWriter struct{}

func (shortJSONLWriter) Write(p []byte) (int, error) { return len(p) - 1, nil }

func TestHeadlessJSONLRejectsShortWrites(t *testing.T) {
	for _, notes := range [][]scriptedNote{
		{completedTurn()},
		{itemNote("th-1", "u1", `{"type":"agentMessage","id":"a","text":"hello"}`), completedTurn()},
	} {
		var stderr bytes.Buffer
		env := CLIEnv{
			Product: config.ProductDal, Dial: newScriptedDaemon(t, notes...).dial,
			Stdout: shortJSONLWriter{}, Stderr: &stderr,
		}
		if code := RunCLI(t.Context(), env, []string{"-p", "review", "--json"}); code != 1 || !strings.Contains(stderr.String(), io.ErrShortWrite.Error()) {
			t.Fatalf("exit %d, stderr %q, want short-write failure", code, stderr.String())
		}
	}
}

func TestHeadlessJSONLPrintsBeforeTurnCompletionAndCancels(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		ctx, cancel := context.WithCancel(t.Context())
		defer cancel()
		daemon := newScriptedDaemon(t, itemNote("th-1", "u1", `{"type":"agentMessage","id":"a","text":"hello","extension":{"kept":true}}`))
		var stdout, stderr bytes.Buffer
		env := CLIEnv{Product: config.ProductDal, Dial: daemon.dial, Stdout: &stdout, Stderr: &stderr}
		done := make(chan int, 1)
		go func() { done <- RunCLI(ctx, env, []string{"-p", "review", "--json"}) }()
		synctest.Wait()
		lines := jsonlLines(t, stdout.String())
		if len(lines) != 1 || !strings.Contains(lines[0], `"extension":{"kept":true}`) {
			t.Fatalf("before completion: %q, want one item with its extension", stdout.String())
		}
		select {
		case code := <-done:
			t.Fatalf("run exited %d before completion or cancellation", code)
		default:
		}
		cancel()
		if code := <-done; code != 1 || !strings.Contains(stderr.String(), context.Canceled.Error()) {
			t.Fatalf("cancelled run: exit %d, stderr %q", code, stderr.String())
		}
		if len(jsonlLines(t, stdout.String())) != 1 {
			t.Fatalf("cancellation wrote an invented result: %q", stdout.String())
		}
	})
}

func TestCancelledHeadlessInputWaitsForEOFWithoutSubmitting(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		daemon := newScriptedDaemon(t, completedTurn())
		reader, writer := io.Pipe()
		defer reader.Close()
		defer writer.Close()
		ctx, cancel := context.WithCancel(t.Context())
		var stderr bytes.Buffer
		env := CLIEnv{Product: config.ProductDal, Dial: daemon.dial, Stdin: reader, Stdout: io.Discard, Stderr: &stderr}
		done := make(chan int, 1)
		go func() { done <- RunCLI(ctx, env, []string{"-p", "review"}) }()
		synctest.Wait()
		cancel()
		synctest.Wait()
		select {
		case code := <-done:
			t.Fatalf("stdin read ended before EOF: exit %d", code)
		default:
		}
		if err := writer.Close(); err != nil {
			t.Fatal(err)
		}
		if code := <-done; code != 1 || daemon.dialCount() != 0 || !strings.Contains(stderr.String(), context.Canceled.Error()) {
			t.Fatalf("exit %d, dials %d, stderr %q", code, daemon.dialCount(), stderr.String())
		}
	})
}

func TestHeadlessJSONLNonCompletedExit(t *testing.T) {
	for _, status := range []string{"failed", "interrupted"} {
		t.Run(status, func(t *testing.T) {
			code, stdout, stderr := runHeadless(t, newScriptedDaemon(t, turnNote("th-1", "u1", status)), nil, "-p", "review", "--json")
			var result resultRecord
			if err := json.Unmarshal([]byte(stdout), &result); err != nil {
				t.Fatal(err)
			}
			if code != 1 || stderr != "" || result.Status != status || result.StopReason != status || result.Type != "result" {
				t.Fatalf("exit %d, result %+v, stderr %q", code, result, stderr)
			}
		})
	}
}

func TestPipedStdinReadFailureStopsBeforeDial(t *testing.T) {
	file := stdinFile(t, "unread input")
	if err := file.Close(); err != nil {
		t.Fatal(err)
	}
	daemon := newScriptedDaemon(t, completedTurn())
	code, stdout, stderr := runHeadless(t, daemon, file, "-p", "review")
	if code != 1 || stdout != "" || !strings.Contains(stderr, "read piped input:") || daemon.dialCount() != 0 {
		t.Fatalf("exit %d, stdout %q, stderr %q, dials %d", code, stdout, stderr, daemon.dialCount())
	}
}
