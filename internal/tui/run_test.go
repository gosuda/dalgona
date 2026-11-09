package tui

import (
	"bufio"
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"strings"
	"sync"
	"testing"
	"time"
)

type outputBuffer struct {
	mu      sync.Mutex
	data    bytes.Buffer
	changed chan struct{}
}

func newOutputBuffer() *outputBuffer {
	return &outputBuffer{changed: make(chan struct{}, 1)}
}

func (b *outputBuffer) Write(data []byte) (int, error) {
	b.mu.Lock()
	n, err := b.data.Write(data)
	b.mu.Unlock()
	select {
	case b.changed <- struct{}{}:
	default:
	}
	return n, err
}

func (b *outputBuffer) String() string {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.data.String()
}

func (b *outputBuffer) waitFor(ctx context.Context, t *testing.T, wants ...string) {
	t.Helper()
	for {
		text := b.String()
		missing := ""
		for _, want := range wants {
			if !strings.Contains(text, want) {
				missing = want
				break
			}
		}
		if missing == "" {
			return
		}
		select {
		case <-b.changed:
		case <-ctx.Done():
			t.Fatalf("terminal output missing %q: %v\n%s", missing, ctx.Err(), text)
		}
	}
}

func TestRunStreamsTurnAndAnswersApproval(t *testing.T) {
	t.Parallel()
	client, server := net.Pipe()
	input, keyboard := io.Pipe()
	defer keyboard.Close()
	ctx, cancel := context.WithTimeoutCause(t.Context(), 5*time.Second, errors.New("tui integration timed out"))
	defer cancel()
	output := newOutputBuffer()
	statuses := make(chan Status, 256)
	children := make(chan [2]string, 4)
	opts := Options{
		Input:  input,
		Output: output,
		Width:  80,
		Height: 24,
		OnStatus: func(status Status) {
			select {
			case statuses <- status:
			case <-ctx.Done():
			}
		},
		OnChildSpawned: func(childID, cwd string) {
			select {
			case children <- [2]string{childID, cwd}:
			case <-ctx.Done():
			}
		},
	}
	runResult := make(chan error, 1)
	go func() { runResult <- Run(ctx, client, opts) }()
	serverResult := make(chan error, 1)
	go func() { serverResult <- serveTurn(server) }()

	output.waitFor(ctx, t, "approval required", "go test ./...")
	waitStatus(ctx, t, statuses, stateAwaitingApproval, "t1")
	if _, err := keyboard.Write([]byte("y")); err != nil {
		t.Fatalf("write approval key: %v", err)
	}
	waitStatus(ctx, t, statuses, stateIdle, "t1")
	output.waitFor(ctx, t, "settled")
	if _, err := keyboard.Write([]byte("\x03\x03")); err != nil {
		t.Fatalf("write quit keys: %v", err)
	}
	if err := <-runResult; err != nil {
		t.Fatalf("Run returned %v", err)
	}
	if err := <-serverResult; err != nil {
		t.Fatalf("fake server returned %v", err)
	}
	assertChildSpawned(t, children)
}

func assertChildSpawned(t *testing.T, children chan [2]string) {
	t.Helper()
	close(children)
	var spawned [][2]string
	for child := range children {
		spawned = append(spawned, child)
	}
	if len(spawned) != 1 || spawned[0] != [2]string{"child-1", "/work/child"} {
		t.Fatalf("OnChildSpawned calls = %v, want one call for child-1 in /work/child", spawned)
	}
}

func waitStatus(ctx context.Context, t *testing.T, statuses <-chan Status, state, threadID string) {
	t.Helper()
	for {
		select {
		case status := <-statuses:
			if status.State == state && status.ThreadID == threadID {
				return
			}
		case <-ctx.Done():
			t.Fatalf("status %q for %s was not delivered: %v", state, threadID, ctx.Err())
		}
	}
}

func serveTurn(conn net.Conn) error {
	defer conn.Close()
	reader := bufio.NewReader(conn)
	if err := answerRequest(conn, reader, "initialize", `{"userAgent":"dalgona/test","codexHome":"/tmp","platformFamily":"unix","platformOs":"linux"}`); err != nil {
		return err
	}
	if err := readNotification(reader, "initialized"); err != nil {
		return err
	}
	if err := answerRequest(conn, reader, "dalgona/keywords/list", `[]`); err != nil {
		return err
	}
	threadStart := `{"approvalPolicy":"on-request","approvalsReviewer":"user","cwd":"/work","model":"gpt-test",` +
		`"modelProvider":"openai","sandbox":{"type":"workspaceWrite"},"thread":` + schemaThread + `}`
	if err := answerRequest(conn, reader, "thread/start", threadStart); err != nil {
		return err
	}
	before := []string{
		`{"jsonrpc":"2.0","method":"thread/started","params":{"thread":` + schemaThread + `}}`,
		`{"jsonrpc":"2.0","method":"turn/started","params":{"threadId":"t1","turn":{"id":"turn1","items":[],"status":"inProgress"}}}`,
		`{"jsonrpc":"2.0","method":"dalgona/thread/childSpawned","params":{"threadId":"other","childId":"child-0","cwd":"/elsewhere"}}`,
		`{"jsonrpc":"2.0","method":"dalgona/thread/childSpawned","params":{"threadId":"t1","childId":"child-1","cwd":"/work/child"}}`,
		`{"jsonrpc":"2.0","method":"item/started","params":{"threadId":"t1","turnId":"turn1","startedAtMs":1,"item":{"type":"agentMessage","id":"a1","text":""}}}`,
		`{"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"threadId":"t1","turnId":"turn1","itemId":"a1","delta":"hello"}}`,
		`{"jsonrpc":"2.0","id":"approval","method":"item/commandExecution/requestApproval","params":{"threadId":"t1","turnId":"turn1","itemId":"cmd1","startedAtMs":2,"command":"go test ./..."}}`,
	}
	if err := writeMessages(conn, before); err != nil {
		return err
	}
	if err := readApprovalResponse(reader); err != nil {
		return err
	}
	after := []string{
		`{"jsonrpc":"2.0","method":"item/completed","params":{"threadId":"t1","turnId":"turn1","completedAtMs":3,"item":{"type":"agentMessage","id":"a1","text":"hello, settled answer"}}}`,
		`{"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":"t1","turn":{"id":"turn1","items":[],"status":"completed"}}}`,
	}
	if err := writeMessages(conn, after); err != nil {
		return err
	}
	if _, err := io.Copy(io.Discard, reader); err != nil {
		return fmt.Errorf("drain client after turn: %w", err)
	}
	return nil
}

func readNotification(reader *bufio.Reader, method string) error {
	line, err := reader.ReadString('\n')
	if err != nil {
		return fmt.Errorf("read %s notification: %w", method, err)
	}
	if !strings.Contains(line, `"method":"`+method+`"`) || strings.Contains(line, `"id":`) {
		return fmt.Errorf("message = %s, want %s notification", line, method)
	}
	return nil
}

func answerRequest(conn net.Conn, reader *bufio.Reader, method, result string) error {
	id, err := readRequest(reader, method)
	if err != nil {
		return err
	}
	return writeServerLine(conn, `{"jsonrpc":"2.0","id":`+id+`,"result":`+result+`}`)
}

func readRequest(reader *bufio.Reader, method string) (string, error) {
	line, err := reader.ReadString('\n')
	if err != nil {
		return "", fmt.Errorf("read %s request: %w", method, err)
	}
	if !strings.Contains(line, `"method":"`+method+`"`) {
		return "", fmt.Errorf("request = %s, want %s", line, method)
	}
	id := requestID(line)
	if id == "" {
		return "", fmt.Errorf("request %s has no id", line)
	}
	return id, nil
}

func requestID(line string) string {
	const marker = `"id":`
	start := strings.Index(line, marker)
	if start < 0 {
		return ""
	}
	start += len(marker)
	end := strings.IndexAny(line[start:], ",}")
	if end < 0 {
		return ""
	}
	return line[start : start+end]
}

func readApprovalResponse(reader *bufio.Reader) error {
	line, err := reader.ReadString('\n')
	if err != nil {
		return fmt.Errorf("read approval response: %w", err)
	}
	if !strings.Contains(line, `"id":"approval"`) || !strings.Contains(line, `"decision":"accept"`) {
		return fmt.Errorf("approval response = %s, want accept", line)
	}
	return nil
}

func writeMessages(writer io.Writer, messages []string) error {
	for _, message := range messages {
		if err := writeServerLine(writer, message); err != nil {
			return err
		}
	}
	return nil
}

func writeServerLine(writer io.Writer, line string) error {
	if _, err := fmt.Fprintln(writer, line); err != nil {
		return fmt.Errorf("write server message: %w", err)
	}
	return nil
}
