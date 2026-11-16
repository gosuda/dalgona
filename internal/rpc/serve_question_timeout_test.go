package rpc

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"errors"
	"io"
	"log/slog"
	"strings"
	"sync"
	"testing"
	"testing/synctest"
	"time"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/config"
	asktool "github.com/gosuda/dalgona/internal/dal/tool/ask"
)

func waitThreadIdle(t *testing.T, thread *agent.Thread) {
	t.Helper()
	deadline := time.After(waitLimit)
	for {
		status, err := thread.Status()
		if err != nil {
			t.Fatalf("Status: %v", err)
		}
		if !status.Busy {
			return
		}
		select {
		case <-time.After(10 * time.Millisecond):
		case <-deadline:
			t.Fatal("thread did not become idle")
		}
	}
}

func toolCompleted(t *testing.T, thread *agent.Thread, callID string) event.ToolCompleted {
	t.Helper()
	records, _, err := thread.Records()
	if err != nil {
		t.Fatalf("Records: %v", err)
	}
	for _, record := range records {
		if completed, ok := record.Event.(event.ToolCompleted); ok && completed.CallID == callID {
			return completed
		}
	}
	t.Fatalf("no tool.completed event for %s", callID)
	return event.ToolCompleted{}
}

type memLink struct {
	mu     sync.Mutex
	buf    []byte
	once   sync.Once
	peer   *memLink
	wake   chan struct{}
	closed chan struct{}
}

func memPipe() (*memLink, *memLink) {
	a := &memLink{wake: make(chan struct{}, 1), closed: make(chan struct{})}
	b := &memLink{wake: make(chan struct{}, 1), closed: make(chan struct{})}
	a.peer, b.peer = b, a
	return a, b
}

func (m *memLink) Read(p []byte) (int, error) {
	for {
		m.mu.Lock()
		if len(m.buf) > 0 {
			n := copy(p, m.buf)
			m.buf = m.buf[n:]
			m.mu.Unlock()
			return n, nil
		}
		m.mu.Unlock()
		select {
		case <-m.wake:
		case <-m.closed:
			return 0, io.EOF
		}
	}
}

func (m *memLink) Write(p []byte) (int, error) {
	q := bytes.Clone(p)
	m.peer.mu.Lock()
	m.peer.buf = append(m.peer.buf, q...)
	m.peer.mu.Unlock()
	select {
	case m.peer.wake <- struct{}{}:
	default:
	}
	return len(p), nil
}

func (m *memLink) Close() error {
	m.once.Do(func() { close(m.closed) })
	m.peer.once.Do(func() { close(m.peer.closed) })
	return nil
}

type bubbleWindow struct {
	conn     *Conn
	client   *Conn
	requests chan struct{}
	answers  chan any
	resolved chan string
	thread   *agent.Thread
	server   *server
	watch    *watch
}

type windowHandler struct {
	t *testing.T
	w *bubbleWindow
}

func (h windowHandler) Notification(method string, params jsontext.Value) {
	if method != "serverRequest/resolved" {
		return
	}
	h.w.resolved <- fieldOf(h.t, params, "resolution")
}

func (h windowHandler) Request(ctx context.Context, method string, _ jsontext.Value) (any, error) {
	if method != methodRequestUserInput && method != "item/commandExecution/requestApproval" {
		return nil, nil
	}
	h.w.requests <- struct{}{}
	select {
	case answer := <-h.w.answers:
		if err, ok := answer.(error); ok {
			return nil, err
		}
		return answer, nil
	case <-ctx.Done():
		return nil, ctx.Err()
	}
}

func openBubbleWindow(t *testing.T, ctx context.Context, r *rig, thread *agent.Thread) *bubbleWindow {
	t.Helper()
	serverEnd, clientEnd := memPipe()
	w := &bubbleWindow{
		conn: NewConn(ctx, serverEnd, nil), requests: make(chan struct{}, 1),
		answers: make(chan any, 1), resolved: make(chan string, 4), thread: thread,
	}
	client := Dial(clientEnd, windowHandler{t: t, w: w})
	w.client = client
	w.server = &server{
		host: r.host, log: slog.New(slog.DiscardHandler), conn: w.conn,
		watches: map[string]*watch{}, pending: map[string]context.CancelFunc{}, refused: map[string]struct{}{},
	}
	w.watch = &watch{thread: thread}
	w.watch.tracker = w.server.newTracker(thread.ID())
	w.watch.tracker.sink = liveSink{s: w.server, thread: thread.ID()}
	w.server.watches[thread.ID()] = w.watch
	updates := thread.Subscribe()
	if !w.watch.setChannel(updates) {
		t.Fatal("setChannel failed")
	}
	w.pump(ctx, updates)
	return w
}

func (w *bubbleWindow) pump(ctx context.Context, updates <-chan agent.Update) {
	go func() {
		for update := range updates {
			if update.Kind == agent.UpdateQuestion {
				w.server.question(ctx, w.watch, updates, update)
				continue
			}
			if update.Kind == agent.UpdateApproval {
				w.server.ask(ctx, w.watch, updates, update)
			}
		}
	}()
}

func (w *bubbleWindow) resubscribe(ctx context.Context, t *testing.T) {
	t.Helper()
	previous := w.watch.channel()
	fresh := w.thread.Subscribe()
	if !w.watch.setChannel(fresh) {
		t.Fatal("setChannel failed")
	}
	w.thread.Unsubscribe(previous)
	w.pump(ctx, fresh)
}

func (w *bubbleWindow) awaitRequest(t *testing.T) {
	t.Helper()
	select {
	case <-w.requests:
	case <-time.After(waitLimit):
		t.Fatal("the window received no request")
	}
}

func postgresAnswer() map[string]any {
	return map[string]any{"answers": map[string]any{"q1": map[string]any{"answers": []string{"Postgres"}}}}
}

func TestServeQuestionTimedOutResolution(t *testing.T) {
	t.Run("both windows receive timedOut at the timeout", func(t *testing.T) {
		r := newDalgonaRig(t)
		synctest.Test(t, func(t *testing.T) {
			s := twoWindowThread(t, r)
			results := askInBackground(s.ctx, s.thread)
			<-s.first.requests
			<-s.second.requests
			time.Sleep(5 * time.Minute)
			synctest.Wait()
			if reply := awaitAsk(t, results); reply.Outcome != asktool.OutcomeTimedOut {
				t.Fatalf("outcome = %v, want timed out", reply.Outcome)
			}
			requireWindowResolutions(t, s.first, s.second, "timedOut")
		})
	})
	t.Run("an answer one tick before expiry gives answered", func(t *testing.T) {
		r := newDalgonaRig(t)
		synctest.Test(t, func(t *testing.T) {
			s := twoWindowThread(t, r)
			results := askInBackground(s.ctx, s.thread)
			<-s.first.requests
			<-s.second.requests
			time.Sleep(5*time.Minute - time.Nanosecond)
			s.first.answers <- postgresAnswer()
			reply := awaitAsk(t, results)
			if reply.Outcome != asktool.OutcomeAnswered || reply.Answers[0][0] != "Postgres" {
				t.Fatalf("reply = %+v, want Postgres", reply)
			}
			requireWindowResolutions(t, s.first, s.second, "answered")
		})
	})
}

type bubbleSession struct {
	ctx    context.Context
	thread *agent.Thread
	first  *bubbleWindow
	second *bubbleWindow
}

func startWindowThread(t *testing.T, r *rig) (context.Context, *agent.Thread) {
	t.Helper()
	ctx, cancel := context.WithCancel(context.Background())
	t.Cleanup(cancel)
	thread, err := r.host.Start(ctx, agent.ThreadSpec{Cwd: r.cwd})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := thread.Close(); err != nil {
			t.Fatalf("thread.Close: %v", err)
		}
	})
	return ctx, thread
}

func oneWindowThread(t *testing.T, r *rig) bubbleSession {
	t.Helper()
	ctx, thread := startWindowThread(t, r)
	first := openBubbleWindow(t, ctx, r, thread)
	t.Cleanup(func() { _ = first.conn.Close() })
	return bubbleSession{ctx: ctx, thread: thread, first: first}
}

func twoWindowThread(t *testing.T, r *rig) bubbleSession {
	t.Helper()
	ctx, thread := startWindowThread(t, r)
	first := openBubbleWindow(t, ctx, r, thread)
	second := openBubbleWindow(t, ctx, r, thread)
	t.Cleanup(func() { _ = first.conn.Close() })
	t.Cleanup(func() { _ = second.conn.Close() })
	return bubbleSession{ctx: ctx, thread: thread, first: first, second: second}
}

func requireWindowResolutions(t *testing.T, first, second *bubbleWindow, want string) {
	t.Helper()
	for _, window := range []*bubbleWindow{first, second} {
		if resolution := <-window.resolved; resolution != want {
			t.Fatalf("resolution = %q, want %q", resolution, want)
		}
	}
}

func openQuestions(t *testing.T, thread *agent.Thread) int {
	t.Helper()
	status, err := thread.Status()
	if err != nil {
		t.Fatalf("Status: %v", err)
	}
	return status.Questions
}

func TestServeQuestionRefusalAfterResubscribeDeclines(t *testing.T) {
	t.Run("an error reply after the window resubscribed declines", func(t *testing.T) {
		r := newDalgonaRig(t)
		synctest.Test(t, func(t *testing.T) {
			s := oneWindowThread(t, r)
			results := askInBackground(s.ctx, s.thread)
			s.first.awaitRequest(t)
			s.first.resubscribe(s.ctx, t)
			synctest.Wait()
			s.first.answers <- errors.New("window cannot answer")
			synctest.Wait()
			if reply := awaitAsk(t, results); reply.Outcome != asktool.OutcomeNoClient {
				t.Fatalf("outcome = %v, want no client after the resubscribed window refused", reply.Outcome)
			}
		})
	})
	t.Run("a normal answer after the window resubscribed still gives answered", func(t *testing.T) {
		r := newDalgonaRig(t)
		synctest.Test(t, func(t *testing.T) {
			s := oneWindowThread(t, r)
			results := askInBackground(s.ctx, s.thread)
			s.first.awaitRequest(t)
			s.first.resubscribe(s.ctx, t)
			synctest.Wait()
			s.first.answers <- postgresAnswer()
			synctest.Wait()
			if reply := awaitAsk(t, results); reply.Outcome != asktool.OutcomeAnswered {
				t.Fatalf("outcome = %v, want answered", reply.Outcome)
			}
		})
	})
}

func TestServeQuestionRefusalBeforeResubscribeSurvives(t *testing.T) {
	t.Run("a refusal given before the resubscribe still counts", func(t *testing.T) {
		r := newDalgonaRig(t)
		synctest.Test(t, func(t *testing.T) {
			s := twoWindowThread(t, r)
			results := askInBackground(s.ctx, s.thread)
			s.first.awaitRequest(t)
			s.second.awaitRequest(t)
			s.first.answers <- errors.New("window cannot answer")
			synctest.Wait()
			s.first.resubscribe(s.ctx, t)
			synctest.Wait()
			if open := openQuestions(t, s.thread); open != 1 {
				t.Fatalf("open questions while one window can still answer = %d, want 1", open)
			}
			s.second.answers <- errors.New("window cannot answer")
			synctest.Wait()
			if reply := awaitAsk(t, results); reply.Outcome != asktool.OutcomeNoClient {
				t.Fatalf("outcome = %v, want no client after both windows refused", reply.Outcome)
			}
		})
	})
	t.Run("the other window can still answer after the first one resubscribed", func(t *testing.T) {
		r := newDalgonaRig(t)
		synctest.Test(t, func(t *testing.T) {
			s := twoWindowThread(t, r)
			results := askInBackground(s.ctx, s.thread)
			s.first.awaitRequest(t)
			s.second.awaitRequest(t)
			s.first.answers <- errors.New("window cannot answer")
			synctest.Wait()
			s.first.resubscribe(s.ctx, t)
			synctest.Wait()
			s.second.answers <- postgresAnswer()
			synctest.Wait()
			if reply := awaitAsk(t, results); reply.Outcome != asktool.OutcomeAnswered {
				t.Fatalf("outcome = %v, want answered", reply.Outcome)
			}
		})
	})
}

func TestApprovalRefusalAfterResubscribeDeclines(t *testing.T) {
	t.Run("an error reply after the window resubscribed declines the command", func(t *testing.T) {
		thread, window := approvalWindowUnderPrompt(t)
		window.resubscribe(t.Context(), t)
		window.answers <- errors.New("window cannot answer")
		waitThreadIdle(t, thread)
		completed := toolCompleted(t, thread, "call-exec")
		if completed.OK || !strings.Contains(completed.Output, "declined") || strings.Contains(completed.Output, "held") {
			t.Fatalf("exec completed ok=%v output=%q, want declined without running", completed.OK, completed.Output)
		}
	})
	t.Run("an accept after the window resubscribed still runs the command", func(t *testing.T) {
		thread, window := approvalWindowUnderPrompt(t)
		window.resubscribe(t.Context(), t)
		window.answers <- map[string]any{"decision": "accept"}
		waitThreadIdle(t, thread)
		completed := toolCompleted(t, thread, "call-exec")
		if !completed.OK || !strings.Contains(completed.Output, "held") {
			t.Fatalf("exec completed ok=%v output=%q, want the command output", completed.OK, completed.Output)
		}
	})
}

func approvalWindowUnderPrompt(t *testing.T) (*agent.Thread, *bubbleWindow) {
	t.Helper()
	r := newRig(t, config.ApprovalAsk)
	r.llm.push(
		llmReply{body: sseCall("call-exec", "exec", `{"command":"echo held"}`)},
		llmReply{body: sseText("done")},
	)
	thread, err := r.host.Start(t.Context(), agent.ThreadSpec{Cwd: r.cwd})
	if err != nil {
		t.Fatal(err)
	}
	window := openBubbleWindow(t, t.Context(), r, thread)
	t.Cleanup(func() { _ = window.conn.Close() })
	if err := thread.Submit(t.Context(), agent.Input{Kind: agent.InputPrompt, Text: "run it"}); err != nil {
		t.Fatalf("Submit: %v", err)
	}
	window.awaitRequest(t)
	return thread, window
}
