package app

import (
	"bytes"
	"cmp"
	"context"
	"encoding/json/jsontext"
	"encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"strings"
	"sync"

	"github.com/gosuda/dalgona/internal/rpc"
)

func resultOf(turn rpc.Turn) turnResult {
	result := turnResult{status: turn.Status, stop: "stop"}
	switch {
	case turn.Error != nil:
		result.stop = turn.Error.Message
	case turn.Status != "completed":
		result.stop = turn.Status
	}
	for _, it := range turn.Items {
		if it.Type == "agentMessage" && it.Text != "" {
			result.texts = append(result.texts, it.Text)
		}
	}
	return result
}

func (env CLIEnv) headless(ctx context.Context, f cliFlags) int {
	session, err := env.connect(ctx)
	if err != nil {
		return env.exit(err)
	}
	defer session.close(env)
	opened, err := session.open(ctx, f)
	if err != nil {
		return env.exit(err)
	}
	result, err := session.settle(ctx, opened, f.prompt)
	if err != nil {
		return env.exit(err)
	}
	return env.report(opened.id, result, f.json)
}

func (s *cliSession) settle(ctx context.Context, opened openedThread, prompt string) (turnResult, error) {
	if opened.active != "" {
		result, err := s.book.wait(ctx, opened.active)
		if err != nil || prompt == "" {
			return result, err
		}
	}
	if prompt != "" {
		return s.turn(ctx, opened.id, prompt)
	}
	if opened.last == nil {
		return turnResult{}, fmt.Errorf("thread %s has no turn", opened.id)
	}
	return *opened.last, nil
}

func (s *cliSession) turn(ctx context.Context, threadID, prompt string) (turnResult, error) {
	var started rpc.TurnStartResponse
	params := rpc.TurnStartParams{ThreadID: threadID, Input: []rpc.InputItem{{Type: "text", Text: prompt}}}
	if err := s.client.Call(ctx, "turn/start", params, &started); err != nil {
		return turnResult{}, err
	}
	return s.book.wait(ctx, started.Turn.ID)
}

func (env CLIEnv) report(threadID string, result turnResult, asJSON bool) int {
	if asJSON {
		line, err := statusLine(threadID, result)
		if err != nil {
			return env.exit(err)
		}
		fmt.Fprintf(env.Stdout, "%s\n", line)
	} else if text := strings.Join(result.texts, "\n\n"); text != "" {
		fmt.Fprintf(env.Stdout, "%s\n", text)
	}
	if result.status != "completed" {
		if !asJSON {
			fmt.Fprintf(env.Stderr, "%s: turn %s: %s\n", env.Product, result.status, result.stop)
		}
		return 1
	}
	return 0
}

func statusLine(threadID string, result turnResult) ([]byte, error) {
	var buf bytes.Buffer
	enc := jsontext.NewEncoder(&buf)
	tokens := []jsontext.Token{
		jsontext.BeginObject,
		jsontext.String("thread"), jsontext.String(threadID),
		jsontext.String("status"), jsontext.String(result.status),
		jsontext.String("stop_reason"), jsontext.String(result.stop),
		jsontext.EndObject,
	}
	for _, token := range tokens {
		if err := enc.WriteToken(token); err != nil {
			return nil, fmt.Errorf("encode status line: %w", err)
		}
	}
	return bytes.TrimSuffix(buf.Bytes(), []byte{'\n'}), nil
}

type turnResult struct {
	status string
	stop   string
	texts  []string
}

type turnBook struct {
	stderr     io.Writer
	mu         sync.Mutex
	changed    chan struct{}
	parkNotify chan struct{}
	done       map[string]turnResult
	texts      map[string][]string
	logins     map[string]rpc.AccountLoginCompletedNotification
	lost       <-chan struct{}
}

func newTurnBook(stderr io.Writer) *turnBook {
	return &turnBook{
		stderr: stderr, changed: make(chan struct{}), done: map[string]turnResult{}, texts: map[string][]string{},
		logins: map[string]rpc.AccountLoginCompletedNotification{},
	}
}

func (b *turnBook) Notification(method string, params jsontext.Value) {
	switch method {
	case "item/completed":
		var note rpc.ItemCompletedNotification
		if json.Unmarshal(params, &note) == nil && note.Item.Type == "agentMessage" && note.Item.Text != "" {
			b.mu.Lock()
			b.texts[note.TurnID] = append(b.texts[note.TurnID], note.Item.Text)
			b.mu.Unlock()
		}
	case "turn/completed":
		var note rpc.TurnCompletedNotification
		if json.Unmarshal(params, &note) == nil {
			b.finish(note.Turn)
		}
	case "account/login/completed":
		var note rpc.AccountLoginCompletedNotification
		if json.Unmarshal(params, &note) == nil {
			b.mu.Lock()
			b.logins[note.LoginID] = note
			b.signal()
			b.mu.Unlock()
		}
	}
}

func (b *turnBook) finish(turn rpc.Turn) {
	result := resultOf(turn)
	b.mu.Lock()
	defer b.mu.Unlock()
	result.texts = b.texts[turn.ID]
	b.done[turn.ID] = result
	b.signal()
}

func (b *turnBook) signal() {
	close(b.changed)
	b.changed = make(chan struct{})
}

func (b *turnBook) Request(_ context.Context, method string, params jsontext.Value) (any, error) {
	if method == "item/tool/requestUserInput" {
		return nil, b.refuseQuestion(params)
	}
	if !strings.HasSuffix(method, "/requestApproval") {
		return nil, fmt.Errorf("unsupported server request %s", method)
	}
	var ask rpc.CommandExecutionRequestApprovalParams
	if err := json.Unmarshal(params, &ask); err != nil {
		return nil, err
	}
	_, _ = fmt.Fprintf(b.stderr, "declined %s: no interactive client\n", cmp.Or(ask.Command, ask.Reason, ask.ItemID))
	return rpc.CommandExecutionRequestApprovalResponse{Decision: "decline"}, nil
}

var errNoInteractiveClient = errors.New("no interactive client")

func (b *turnBook) refuseQuestion(params jsontext.Value) error {
	var ask rpc.ToolRequestUserInputParams
	if err := json.Unmarshal(params, &ask); err != nil {
		return err
	}
	header := ""
	if len(ask.Questions) > 0 {
		header = ask.Questions[0].Header
	}
	fmt.Fprintf(b.stderr, "declined question %s: no interactive client\n", header)
	return errNoInteractiveClient
}

func (b *turnBook) wait(ctx context.Context, turnID string) (turnResult, error) {
	var result turnResult
	err := b.await(ctx, func() (ok bool) {
		result, ok = b.done[turnID]
		return ok
	})
	return result, err
}

func (b *turnBook) waitLogin(ctx context.Context, loginID string) (rpc.AccountLoginCompletedNotification, error) {
	var note rpc.AccountLoginCompletedNotification
	err := b.await(ctx, func() (ok bool) {
		note, ok = b.logins[loginID]
		return ok
	})
	return note, err
}

func (b *turnBook) await(ctx context.Context, ready func() bool) error {
	for {
		ok, changed := b.check(ready)
		if ok {
			return nil
		}
		b.notifyParked()
		select {
		case <-changed:
		case <-b.lost:
			if ok, _ := b.check(ready); ok {
				return nil
			}
			return errConnectionLost
		case <-ctx.Done():
			return ctx.Err()
		}
	}
}

func (b *turnBook) notifyParked() {
	if b.parkNotify == nil {
		return
	}
	select {
	case b.parkNotify <- struct{}{}:
	default:
	}
}

func (b *turnBook) check(ready func() bool) (bool, <-chan struct{}) {
	b.mu.Lock()
	defer b.mu.Unlock()
	return ready(), b.changed
}
