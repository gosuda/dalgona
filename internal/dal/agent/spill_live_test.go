package agent

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/runner"
)

type pemExitRunner struct {
	runner.Runner
	output string
}

func (r pemExitRunner) ReadPage(_ context.Context, _ string, offset, limit int64) (runner.Output, error) {
	end := min(offset+limit, int64(len(r.output)))
	return runner.Output{Data: []byte(r.output[offset:end]), Next: end, Exit: &runner.Exit{}, Path: spillFixturePath}, nil
}

func (r pemExitRunner) Stat(context.Context, string) (runner.FileInfo, error) {
	return runner.FileInfo{Size: int64(len(r.output))}, nil
}

func TestExecExitPagesDoNotPersistUnverifiedPEM(t *testing.T) {
	const progress = "ordinary progress 🚀\n"
	w, home := spillTestWorker(t)
	const header = "-----BEGIN " + "RSA PRIVATE KEY-----\n"
	first := progress + header + strings.Repeat("Z", spillFixturePage-len(header)-len(progress)-1) + "\n"
	second := "private continuation\n-----END " + "RSA PRIVATE KEY-----\n"
	w.t.runner = pemExitRunner{output: first + second}
	assertVerifiedExecSeed(t, w.t, home, progress)
	text, spill, err := spillCompletion(t.Context(), w)
	if err != nil || spill == nil {
		t.Fatalf("masked completion spill = %v, %v", spill, err)
	}
	const masked = progress + "{{secret:builtin7:0}}\n"
	data, err := w.t.host.journal.Blob(spill.SHA256)
	if err != nil || string(data) != masked || !strings.HasPrefix(text, masked) {
		t.Fatalf("masked completion = %d bytes, blob error %v; want only the PEM placeholder and notice", len(data), err)
	}
}

func assertVerifiedExecSeed(t *testing.T, thread *Thread, home, progress string) {
	t.Helper()
	thread.output.ch = make(chan Update, outputBuffer)
	state := event.State{TurnID: "turn", InFlight: map[string]event.ToolState{"exec": {CallID: "exec"}}}
	updates := make(chan Update, 4)
	a := &actor{t: thread, state: state, subs: map[chan Update]struct{}{updates: {}}}
	t.Cleanup(a.activeDeltas.clear)
	tee := thread.teeOutput(thread.runner, "exec", "exec")
	offsets := []int64{0, 0, spillFixturePage, spillFixturePage + 6}
	limits := []int64{int64(len(progress)) + 5, spillFixturePage, 6, spillFixturePage}
	for i, offset := range offsets {
		readVerifiedPage(t, tee, offset, limits[i])
		a.drainOutput()
		assertExecSeedPrefix(t, a, home, progress)
	}
	close(updates)
	var visible strings.Builder
	for update := range updates {
		visible.WriteString(update.Delta)
	}
	if visible.String() != progress {
		t.Fatalf("live subscription received %d bytes, want only %d verified bytes", visible.Len(), len(progress))
	}
}

func assertExecSeedPrefix(t *testing.T, a *actor, home, progress string) {
	t.Helper()
	entries, err := os.ReadDir(filepath.Join(home, "scratch"))
	if err != nil || len(entries) != 0 || len(a.activeDeltas.items) != 1 {
		t.Fatalf("seed has %d bodies and %d scratch files: %v", len(a.activeDeltas.items), len(entries), err)
	}
	body := a.activeDeltas.items[0]
	if string(body.memory) != progress || body.stored != 0 {
		t.Fatalf("seed stored %d bytes, want %d verified bytes", len(body.memory)+int(body.stored), len(progress))
	}
}

func readVerifiedPage(t *testing.T, tee *outputTee, offset, limit int64) {
	t.Helper()
	if _, err := tee.ReadPage(t.Context(), "exec", offset, limit); err != nil {
		t.Fatal(err)
	}
}
