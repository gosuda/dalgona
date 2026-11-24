package agent

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/auth"
	"github.com/gosuda/dalgona/internal/dal/journal"
	"github.com/gosuda/dalgona/internal/dal/runner"
	"github.com/gosuda/dalgona/internal/dal/tool"
)

const (
	spillFixturePage      = 1 << 20
	spillFixturePath      = "runs/exec/out"
	spillFixtureSecret    = "demo-page-secret-1234567890"
	spillFixturePattern   = "begin-AABB-secret-end"
	spillFixtureSize      = 33*spillFixturePage + 24
	spillFixtureSecretAt  = spillFixturePage - 5
	spillFixturePatternAt = spillFixtureSize - 31
)

type spillFixture struct {
	runner.Runner
	storage []byte
	cancel  context.CancelFunc
}

func (*spillFixture) Stat(context.Context, string) (runner.FileInfo, error) {
	return runner.FileInfo{Size: spillFixtureSize}, nil
}

func (f *spillFixture) ReadPage(_ context.Context, _ string, offset, limit int64) (runner.Output, error) {
	if limit > spillFixturePage || offset+limit > spillFixtureSize {
		return runner.Output{}, fmt.Errorf("unbounded range: %d + %d", offset, limit)
	}
	if offset > 0 && f.cancel != nil {
		f.cancel()
		return runner.Output{}, context.Canceled
	}
	window := f.storage[:limit]
	for i := range window {
		window[i] = 'q'
	}
	insertSpillFixture(window, offset, spillFixtureSecretAt, spillFixtureSecret)
	insertSpillFixture(window, offset, spillFixturePatternAt, spillFixturePattern)
	return runner.Output{Data: window, Next: offset + limit, Path: spillFixturePath}, nil
}

func insertSpillFixture(window []byte, offset, start int64, value string) {
	if from, to := max(offset, start), min(offset+int64(len(window)), start+int64(len(value))); from < to {
		copy(window[from-offset:to-offset], value[from-start:to-start])
	}
}

func TestExecSpillMasksFullStreamWithBoundedBuffers(t *testing.T) {
	w, home := spillTestWorker(t)
	var before, after runtime.MemStats
	runtime.ReadMemStats(&before)
	text, spill, err := spillCompletion(t.Context(), w)
	runtime.ReadMemStats(&after)
	if err != nil || spill == nil {
		t.Fatalf("spill = %v, %v", spill, err)
	}
	if allocated := after.TotalAlloc - before.TotalAlloc; allocated > 12<<20 {
		t.Fatalf("spill allocated %d bytes for %d bytes of output", allocated, spillFixtureSize)
	}
	assertSpillDigest(t, home, spill)
	assertSpillProjection(t, text)
	assertNoSpillScratch(t, home)
}

func spillTestWorker(t *testing.T) (*worker, string) {
	t.Helper()
	home := t.TempDir()
	scratch := filepath.Join(home, "scratch")
	if err := os.Mkdir(scratch, 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("TMPDIR", scratch)
	t.Setenv("TMP", scratch)
	t.Setenv("DAL_SPILL_FIXTURE_PASSWORD", spillFixtureSecret)
	store, err := journal.Open(home)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if closeErr := store.Close(); closeErr != nil {
			t.Error(closeErr)
		}
	})
	credentials, err := auth.NewWithPatterns(home, []string{`begin-[A-Z]+-secret-end`})
	if err != nil {
		t.Fatal(err)
	}
	host := &Host{cfg: HostConfig{Auth: credentials}, journal: store}
	return &worker{t: &Thread{host: host, runner: &spillFixture{storage: make([]byte, spillFixturePage)}}}, home
}

func spillCompletion(ctx context.Context, w *worker) (string, *event.Spill, error) {
	p := &prepared{call: event.ToolCall{ID: "exec", Name: "exec"}, class: tool.ClassExec}
	return w.completionOutput(ctx, p, tool.Result{Path: spillFixturePath}, nil)
}

func assertSpillDigest(t *testing.T, home string, spill *event.Spill) {
	t.Helper()
	want := sha256.New()
	plain := bytes.Repeat([]byte{'q'}, spillFixturePage)
	writeSpillPadding(t, want, plain, spillFixtureSecretAt)
	_, _ = io.WriteString(want, "{{secret:DAL_SPILL_FIXTURE_PASSWORD}}")
	writeSpillPadding(t, want, plain, spillFixturePatternAt-spillFixtureSecretAt-len(spillFixtureSecret))
	_, _ = io.WriteString(want, "{{secret:pattern0:0}}")
	writeSpillPadding(t, want, plain, spillFixtureSize-spillFixturePatternAt-len(spillFixturePattern))
	got, size := storedSpillDigest(t, filepath.Join(home, spill.Path))
	digest := hex.EncodeToString(want.Sum(nil))
	wantSize := int64(spillFixtureSize - len(spillFixtureSecret) - len(spillFixturePattern) + len("{{secret:DAL_SPILL_FIXTURE_PASSWORD}}{{secret:pattern0:0}}"))
	if size != wantSize || spill.Bytes != wantSize || got != digest || spill.SHA256 != digest || spill.Path != "blobs/"+digest {
		t.Fatalf("stored size=%d digest=%s, ref=%+v; want %d and %s", size, got, spill, wantSize, digest)
	}
}

func writeSpillPadding(t *testing.T, out io.Writer, pad []byte, count int) {
	t.Helper()
	for count > 0 {
		chunk := pad[:min(count, len(pad))]
		if _, err := out.Write(chunk); err != nil {
			t.Fatal(err)
		}
		count -= len(chunk)
	}
}

func storedSpillDigest(t *testing.T, path string) (string, int64) {
	t.Helper()
	file, err := os.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = file.Close() })
	hash := sha256.New()
	size, err := io.Copy(hash, file)
	if err != nil {
		t.Fatal(err)
	}
	return hex.EncodeToString(hash.Sum(nil)), size
}

func assertSpillProjection(t *testing.T, text string) {
	t.Helper()
	tail, status, ok := strings.Cut(text, "\n[output truncated: ")
	wantStatus := fmt.Sprintf("%d bytes total; full output: %s; page it with read offset/limit]", spillFixtureSize, spillFixturePath)
	if !ok || status != wantStatus || len(tail) != 64<<10 || !strings.HasSuffix(tail, "{{secret:pattern0:0}}"+strings.Repeat("q", 31-len(spillFixturePattern))) {
		t.Fatalf("projection has %d bytes of tail and status %q", len(tail), status)
	}
}

func TestExecSpillCancellationRemovesStaging(t *testing.T) {
	w, home := spillTestWorker(t)
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	fixture, ok := w.t.runner.(*spillFixture)
	if !ok {
		t.Fatal("missing spill fixture")
	}
	fixture.cancel = cancel
	if text, spill, err := spillCompletion(ctx, w); !errors.Is(err, context.Canceled) || spill != nil || text != "" {
		t.Fatalf("cancelled spill = %d bytes, %v, %v", len(text), spill, err)
	}
	assertNoSpillScratch(t, home)
	entries, err := os.ReadDir(filepath.Join(home, "blobs"))
	if err != nil || len(entries) != 0 {
		t.Fatalf("cancelled blob entries = %v, %v", entries, err)
	}
}

func assertNoSpillScratch(t *testing.T, home string) {
	t.Helper()
	entries, err := os.ReadDir(filepath.Join(home, "scratch"))
	if err != nil || len(entries) != 0 {
		t.Fatalf("spill scratch entries = %v, %v", entries, err)
	}
}
