package rpcserver

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"io"
	"sync"
	"testing"
	"testing/synctest"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/runner"
)

type scriptedTerminal struct {
	output   chan []byte
	done     chan struct{}
	ended    sync.Once
	exit     runner.Exit
	closed   chan struct{}
	closing  sync.Once
	mu       sync.Mutex
	signals  []runner.Signal
	onSignal func(runner.Signal)
}

func newScriptedTerminal() *scriptedTerminal {
	return &scriptedTerminal{output: make(chan []byte), done: make(chan struct{}), closed: make(chan struct{})}
}

func (p *scriptedTerminal) Read(b []byte) (int, error) {
	select {
	case chunk, ok := <-p.output:
		if !ok {
			return 0, io.EOF
		}
		return copy(b, chunk), nil
	case <-p.closed:
		return 0, io.EOF
	}
}

func (p *scriptedTerminal) Write(b []byte) (int, error) { return len(b), nil }

func (p *scriptedTerminal) Resize(int, int) error { return nil }

func (p *scriptedTerminal) Signal(signal runner.Signal) error {
	p.mu.Lock()
	p.signals = append(p.signals, signal)
	p.mu.Unlock()
	if p.onSignal != nil {
		p.onSignal(signal)
	}
	return nil
}

func (p *scriptedTerminal) signalled() []runner.Signal {
	p.mu.Lock()
	defer p.mu.Unlock()
	return append([]runner.Signal(nil), p.signals...)
}

func (p *scriptedTerminal) Done() <-chan struct{} { return p.done }

func (p *scriptedTerminal) Exit() runner.Exit {
	<-p.done
	return p.exit
}

func (p *scriptedTerminal) Close() error {
	p.closing.Do(func() { close(p.closed) })
	return nil
}

func (p *scriptedTerminal) end(code int) {
	p.ended.Do(func() {
		p.exit = runner.Exit{Code: code, EndedAt: time.Now()}
		close(p.done)
	})
}

type leaseHarness struct {
	t    *testing.T
	host *TerminalHost
	s    *server
	term *terminal
	proc *scriptedTerminal
	att  string
}

func newLeaseHarness(t *testing.T, proc *scriptedTerminal) *leaseHarness {
	t.Helper()
	host := NewTerminalHost(transcriptEngine)
	s := &server{opts: ServeOptions{Terminals: host}}
	s.native.Store(&nativeSession{})
	if err := host.reserve(); err != nil {
		t.Fatal(err)
	}
	term := newTerminal(host, terminalIdentity{threadID: "thread", runnerID: "local", bindingRevision: "1", root: "/work"}, proc, 80, 24)
	term.mu.Lock()
	att, _, err := term.attachLocked(attachRequest{owner: s, workspaceID: "workspace", validate: permitTestTerminal, access: accessWrite}, time.Now())
	term.mu.Unlock()
	if err != nil {
		t.Fatal(err)
	}
	host.insert(term)
	term.start()
	return &leaseHarness{t: t, host: host, s: s, term: term, proc: proc, att: att.id}
}

func permitTestTerminal() error { return nil }

func (h *leaseHarness) call(handler method, params map[string]any) (jsontext.Value, error) {
	h.t.Helper()
	raw, err := json.Marshal(params)
	if err != nil {
		h.t.Fatal(err)
	}
	return handler(h.s, context.Background(), raw)
}

func (h *leaseHarness) read(offset, ordinal string) (terminalPageWire, error) {
	h.t.Helper()
	result, err := h.call((*server).terminalRead, map[string]any{"terminalId": h.term.id, "attachmentId": h.att, "offset": offset, "afterResizeOrdinal": ordinal})
	var page terminalPageWire
	if err == nil {
		err = json.Unmarshal(result, &page)
	}
	return page, err
}

func TestTerminalFinalOutputCollectionEndsAtTheFixedRetentionDeadline(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		proc := newScriptedTerminal()
		proc.onSignal = func(signal runner.Signal) {
			if signal == runner.SignalTerm {
				go func() {
					proc.output <- []byte("final-words")
					proc.end(0)
				}()
			}
		}
		h := newLeaseHarness(t, proc)
		proc.output <- []byte("running;")
		final, expires := closeDuringShutdown(t, h)
		offset, ordinal := collectShutdownOutput(t, h, final)
		checkReadsBeforeExpiry(t, h, offset, ordinal)
		checkGoneAfterRetention(t, h, offset, ordinal, expires)
	})
}

func closeDuringShutdown(t *testing.T, h *leaseHarness) (terminalStatusWire, time.Time) {
	t.Helper()
	started := time.Now()
	result, err := h.call((*server).terminalClose, map[string]any{"terminalId": h.term.id})
	if err != nil {
		t.Fatal(err)
	}
	var closed struct {
		Status terminalStatusWire `json:"status"`
	}
	if decodeErr := json.Unmarshal(result, &closed); decodeErr != nil {
		t.Fatal(decodeErr)
	}
	if elapsed := time.Since(started); elapsed != terminalDrain {
		t.Fatalf("close reported after %v, want the %v drain deadline", elapsed, terminalDrain)
	}
	final := closed.Status
	if final.FinalOffset != "19" || final.OutputTruncated == nil || !*final.OutputTruncated {
		t.Fatalf("final status %+v, want 19 bytes marked truncated by the drain deadline", final)
	}
	expires, err := time.Parse(time.RFC3339, final.OutputExpiresAt)
	if err != nil || !expires.Equal(started.Add(terminalDrain+terminalRetention)) {
		t.Fatalf("output expires at %q, want %v after the drain", final.OutputExpiresAt, terminalRetention)
	}
	return final, expires
}

func collectShutdownOutput(t *testing.T, h *leaseHarness, final terminalStatusWire) (string, string) {
	t.Helper()
	var collected []byte
	offset, ordinal := "0", "0"
	for offset != final.FinalOffset || ordinal != final.FinalResizeOrdinal {
		page, readErr := h.read(offset, ordinal)
		if readErr != nil {
			t.Fatal(readErr)
		}
		data, _ := base64.StdEncoding.DecodeString(page.DataBase64)
		collected = append(collected, data...)
		offset, ordinal = page.Next, page.ResizeOrdinal
	}
	if string(collected) != "running;final-words" {
		t.Fatalf("collected %q, want the output emitted during shutdown", collected)
	}
	return offset, ordinal
}

func checkReadsBeforeExpiry(t *testing.T, h *leaseHarness, offset, ordinal string) {
	t.Helper()
	for range 5 {
		time.Sleep(10 * time.Second)
		if _, readErr := h.read(offset, ordinal); readErr != nil {
			t.Fatalf("read before retention expiry: %v", readErr)
		}
		if _, callErr := h.call((*server).terminalClose, map[string]any{"terminalId": h.term.id}); callErr != nil {
			t.Fatalf("repeated close before retention expiry: %v", callErr)
		}
	}
}

func checkGoneAfterRetention(t *testing.T, h *leaseHarness, offset, ordinal string, expires time.Time) {
	t.Helper()
	time.Sleep(time.Until(expires))
	synctest.Wait()
	_, err := h.read(offset, ordinal)
	if data, ok := rpc.ErrorDataOf(err); !ok || data.Kind != rpc.ErrorExpired {
		t.Fatalf("read after retention expiry returned %v, want expired", err)
	}
	if len(h.host.all()) != 0 || h.host.checkpointBytes != 0 {
		t.Fatalf("retention expiry left %d terminals and %d checkpoint bytes", len(h.host.all()), h.host.checkpointBytes)
	}
}

func TestTerminalLeaseExpiryStopsAnAbandonedTerminalWithoutExtension(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		proc := newScriptedTerminal()
		proc.onSignal = func(runner.Signal) {
			proc.end(0)
			proc.closing.Do(func() { close(proc.closed) })
		}
		h := newLeaseHarness(t, proc)
		time.Sleep(20 * time.Second)
		if _, err := h.read("0", "0"); err != nil {
			t.Fatal(err)
		}
		time.Sleep(5 * time.Second)
		result, err := h.call((*server).terminalDetach, map[string]any{"terminalId": h.term.id, "attachmentId": h.att})
		if err != nil || !bytes.Contains(result, []byte(`"detached"`)) {
			t.Fatalf("detach = %s, %v", result, err)
		}
		time.Sleep(24 * time.Second)
		synctest.Wait()
		if signals := proc.signalled(); len(signals) != 0 {
			t.Fatalf("terminal received %v before its renewed lease ended", signals)
		}
		time.Sleep(2 * time.Second)
		synctest.Wait()
		if signals := proc.signalled(); len(signals) == 0 || signals[0] != runner.SignalTerm {
			t.Fatalf("terminal received %v after its lease ended, want a graceful stop", signals)
		}
		<-h.term.finished
		time.Sleep(terminalRetention)
		synctest.Wait()
		if len(h.host.all()) != 0 {
			t.Fatal("finalized terminal outlived its retention")
		}
	})
}

func TestDetachedNewerLeaseDoesNotExtendLastAttachment(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		proc := newScriptedTerminal()
		proc.onSignal = func(runner.Signal) { proc.end(0) }
		h := newLeaseHarness(t, proc)
		time.Sleep(10 * time.Second)
		h.term.mu.Lock()
		other, _, err := h.term.attachLocked(attachRequest{owner: h.s, workspaceID: "workspace", validate: permitTestTerminal, access: accessRead}, time.Now())
		h.term.mu.Unlock()
		if err != nil {
			t.Fatal(err)
		}
		h.term.detach(h.s, other.id)
		time.Sleep(20 * time.Second)
		synctest.Wait()
		if len(proc.signalled()) == 0 {
			t.Error("terminal outlived the final attachment's deadline")
			proc.end(0)
		}
		time.Sleep(terminalRetention)
	})
}

func TestDetachedCheckpointRetainsCapacityUntilReadCompletes(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		h := newLeaseHarness(t, newScriptedTerminal())
		h.term.mu.Lock()
		att := h.term.attachments[h.att]
		cp, err := h.term.openCheckpointRead(att, att.checkpoint.id, time.Now())
		h.term.mu.Unlock()
		if err != nil {
			t.Fatal(err)
		}
		h.term.detach(h.s, h.att)
		err = h.host.chargeCheckpoint(128 << 20)
		if err == nil {
			t.Error("detaching an admitted read refunded its checkpoint budget")
			h.host.refundCheckpoint(128 << 20)
		}
		h.term.closeCheckpointRead(cp)
		if err := h.host.chargeCheckpoint(128 << 20); err != nil {
			t.Error("completed read did not refund checkpoint budget:", err)
		} else {
			h.host.refundCheckpoint(128 << 20)
		}
		h.proc.end(0)
		time.Sleep(terminalRetention + terminalDrain)
	})
}

type countedCheckpointScreen struct {
	TerminalScreen
	captures int
}

func (s *countedCheckpointScreen) Checkpoint() ([]byte, error) {
	s.captures++
	return s.TerminalScreen.Checkpoint()
}

func TestCheckpointCapacityIsReservedBeforeEncoding(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		h := newLeaseHarness(t, newScriptedTerminal())
		h.term.mu.Lock()
		screen := &countedCheckpointScreen{TerminalScreen: h.term.screen}
		h.term.screen = screen
		old := h.term.attachments[h.att].checkpoint
		h.term.mu.Unlock()
		held := (128 << 20) - len(old.body)
		if err := h.host.chargeCheckpoint(held); err != nil {
			t.Fatal(err)
		}
		params := map[string]any{"terminalId": h.term.id, "attachmentId": h.att}
		_, err := h.call((*server).terminalCheckpointOpen, params)
		h.host.refundCheckpoint(held)
		refusedWith(t, err, rpc.ErrorLimitExceeded)
		if screen.captures != 0 {
			t.Errorf("budget refusal encoded %d replacement checkpoints", screen.captures)
		}
		_, err = h.call((*server).terminalCheckpointRead, map[string]any{
			"terminalId": h.term.id, "attachmentId": h.att, "checkpointId": old.id,
		})
		if err != nil {
			t.Error("budget refusal retired the usable checkpoint:", err)
		}
		h.proc.end(0)
		time.Sleep(terminalRetention + terminalDrain)
	})
}
