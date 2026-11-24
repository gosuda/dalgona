package agent

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/auth"
	"github.com/gosuda/dalgona/internal/dal/runner"
	"github.com/gosuda/dalgona/internal/dal/tool"
	"github.com/gosuda/dalgona/internal/dal/tool/exec"
)

type changingExecSpill struct {
	pemExitRunner
	path        string
	missingStat bool
	growAtStat  string
	shortRead   bool
}

func (*changingExecSpill) Start(context.Context, runner.ProcessSpec) error { return nil }

func (*changingExecSpill) Close() error { return nil }

func (r *changingExecSpill) ReadPage(ctx context.Context, id string, offset, limit int64) (runner.Output, error) {
	page, err := r.pemExitRunner.ReadPage(ctx, id, offset, limit)
	page.Path = r.path
	return page, err
}

func (r *changingExecSpill) Stat(ctx context.Context, path string) (runner.FileInfo, error) {
	if r.missingStat {
		return runner.FileInfo{}, fs.ErrNotExist
	}
	if r.growAtStat != "" {
		size := int64(len(r.growAtStat))
		if !r.shortRead {
			r.output = r.growAtStat
		}
		r.growAtStat = ""
		return runner.FileInfo{Size: size}, nil
	}
	return r.pemExitRunner.Stat(ctx, path)
}

func TestExecCompletionChecksObservedSpillBacking(t *testing.T) {
	pem := "-----BEGIN " + "RSA PRIVATE KEY-----\n" + strings.Repeat("p", 2*event.MaxInlineBytes) +
		"\n-----END " + "RSA PRIVATE KEY-----\n"
	for _, row := range []struct {
		name, path string
		size       int
		wantErr    error
	}{
		{"missing", spillFixturePath, 0, fs.ErrNotExist},
		{"short below inline cutoff", spillFixturePath, event.MaxInlineBytes - 1, io.ErrUnexpectedEOF},
		{"short above inline cutoff", spillFixturePath, event.MaxInlineBytes + 1, io.ErrUnexpectedEOF},
		{"missing path", "", len(pem), io.ErrUnexpectedEOF},
		{"complete", spillFixturePath, len(pem), nil},
	} {
		t.Run(row.name, func(t *testing.T) {
			w, home := spillTestWorker(t)
			run := &changingExecSpill{pemExitRunner: pemExitRunner{output: pem}, path: row.path}
			w.t.runner = run
			p, result := observeSpillExec(t, run)
			run.output, run.missingStat = pem[:row.size], row.size == 0
			completed, err := w.completion(t.Context(), p, result, nil)
			if row.wantErr != nil {
				assertExecSpillRefused(t, home, completed, err, row.wantErr)
				return
			}
			assertCompleteExecPEM(t, w, completed, err)
		})
	}
}

func TestExecCompletionPreservesLegitimateMissingEmptyOutput(t *testing.T) {
	w, _ := spillTestWorker(t)
	run := &changingExecSpill{path: spillFixturePath, missingStat: true}
	w.t.runner = run
	p, result := observeSpillExec(t, run)
	completed, err := w.completion(t.Context(), p, result, nil)
	if err != nil || completed.Spill != nil || completed.Output != "" || !completed.OK {
		t.Fatalf("empty run: OK=%t, %d output bytes, spill=%v, error=%v", completed.OK, len(completed.Output), completed.Spill, err)
	}
}

func TestExecCompletionMasksGrowingInlineBacking(t *testing.T) {
	prefix, full := inlineGrowingPEM()
	for _, row := range []struct {
		name     string
		observed string
		grow     string
		short    bool
	}{
		{name: "complete reread", observed: prefix, grow: full},
		{name: "short reread", observed: prefix, grow: full, short: true},
		{name: "already complete", observed: full},
	} {
		t.Run(row.name, func(t *testing.T) {
			w, home := spillTestWorker(t)
			run := &changingExecSpill{
				pemExitRunner: pemExitRunner{output: row.observed}, path: spillFixturePath,
				growAtStat: row.grow, shortRead: row.short,
			}
			w.t.runner = run
			p, result := observeSpillExec(t, run)
			completed, err := w.completion(t.Context(), p, result, nil)
			if row.short {
				assertExecSpillRefused(t, home, completed, err, io.ErrUnexpectedEOF)
				return
			}
			assertInlineMaskedPEM(t, completed, err)
		})
	}
}

func TestRecoverExecMasksGrowingInlineBacking(t *testing.T) {
	prefix, full := inlineGrowingPEM()
	h := newHarness(t, func(h *harness) {
		credentials, err := auth.Load(h.home)
		if err != nil {
			t.Fatal(err)
		}
		h.cfg.Auth = credentials
		h.cfg.Runner = func(_ context.Context, _, _ string) (runner.Runner, error) {
			return &changingExecSpill{
				pemExitRunner: pemExitRunner{output: prefix}, path: spillFixturePath, growAtStat: full,
			}, nil
		}
	})
	h.model.push(text("done"))
	h.seed(execPrefix("c1", "true")...)
	completed := completedOutputs(h.recoverSession())["c1"]
	assertInlineMaskedPEM(t, completed, nil)
	if completed.Recovered != recoveredReplay {
		t.Fatalf("completion recovery = %q, want %q", completed.Recovered, recoveredReplay)
	}
}

func inlineGrowingPEM() (prefix, full string) {
	prefix = "-----BEGIN " + "RSA PRIVATE KEY-----\nbody-secret\n"
	return prefix, prefix + "-----END " + "RSA PRIVATE KEY-----\n"
}

func assertInlineMaskedPEM(t *testing.T, completed event.ToolCompleted, err error) {
	t.Helper()
	const masked = "{{secret:builtin7:0}}\n"
	if err != nil || completed.Spill != nil || !completed.OK || completed.Output != masked {
		t.Fatalf("wrong inline completion: %d bytes, spill %v, error=%v", len(completed.Output), completed.Spill, err)
	}
}

func observeSpillExec(t *testing.T, run runner.Runner) (*prepared, tool.Result) {
	t.Helper()
	const args = `{"command":"true"}`
	executable := exec.New()
	p := &prepared{call: event.ToolCall{ID: "exec", Name: "exec"}, class: tool.ClassExec, tool: executable}
	result, err := executable.Run(t.Context(), tool.Call{ID: "exec", Args: jsontext.Value(args)}, tool.Env{Runner: run, Cwd: t.TempDir()})
	if err != nil || !result.OK {
		t.Fatalf("exec did not return the observed output: %v", err)
	}
	return p, result
}

func assertExecSpillRefused(t *testing.T, home string, completed event.ToolCompleted, err, want error) {
	t.Helper()
	if !errors.Is(err, want) || completed.Spill != nil || completed.Output != "" {
		t.Fatalf("incomplete output was accepted: %d bytes, spill %v, error %v; want %v", len(completed.Output), completed.Spill, err, want)
	}
	if entries, err := os.ReadDir(filepath.Join(home, "blobs")); err != nil || len(entries) != 0 {
		t.Fatalf("refused spill published %d blob entries: %v", len(entries), err)
	}
}

func assertCompleteExecPEM(t *testing.T, w *worker, completed event.ToolCompleted, err error) {
	t.Helper()
	const masked = "{{secret:builtin7:0}}\n"
	if err != nil || completed.Spill == nil || !completed.OK || !strings.HasPrefix(completed.Output, masked) {
		t.Fatalf("complete spill rejected: %d bytes, spill %v, error %v", len(completed.Output), completed.Spill, err)
	}
	data, err := w.t.host.journal.Blob(completed.Spill.SHA256)
	if err != nil || string(data) != masked {
		t.Fatalf("completed spill stored %d bytes, want just the placeholder; error=%v", len(data), err)
	}
}
