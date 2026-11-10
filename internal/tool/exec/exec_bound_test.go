package exec

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"io/fs"
	"strconv"
	"strings"
	"testing"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
)

const boundKeep = 64 * 1024

type pagedRunner struct {
	data []byte
	page int
	path string
	code int
}

func (f *pagedRunner) Start(context.Context, runner.ProcessSpec) error { return nil }

func (f *pagedRunner) Read(_ context.Context, _ string, offset int64) (runner.Output, error) {
	if offset < 0 || offset > int64(len(f.data)) {
		return runner.Output{}, errors.New("exec: negative output offset")
	}
	if offset == int64(len(f.data)) {
		return runner.Output{Next: offset, Exit: &runner.Exit{Code: f.code}, Path: f.path}, nil
	}
	end := offset + int64(f.page)
	if end >= int64(len(f.data)) {
		return runner.Output{Data: f.data[offset:], Next: int64(len(f.data)), Exit: &runner.Exit{Code: f.code}, Path: f.path}, nil
	}
	return runner.Output{Data: f.data[offset:end], Next: end, Path: f.path}, nil
}

func (f *pagedRunner) Signal(context.Context, string, runner.Signal) error { return nil }

func (f *pagedRunner) ReadFile(context.Context, string) ([]byte, error) {
	return nil, errors.New("exec: paged runner has no files")
}

func (f *pagedRunner) WriteFile(context.Context, string, []byte, fs.FileMode) error {
	return errors.New("exec: paged runner has no files")
}

func (f *pagedRunner) Stat(context.Context, string) (runner.FileInfo, error) {
	return runner.FileInfo{}, errors.New("exec: paged runner has no files")
}

func (f *pagedRunner) ReadDir(context.Context, string) ([]runner.FileInfo, error) {
	return nil, errors.New("exec: paged runner has no files")
}

func (f *pagedRunner) Remove(context.Context, string) error {
	return errors.New("exec: paged runner has no files")
}

func (f *pagedRunner) Close() error { return nil }

func TestExecBoundsOutputToTail(t *testing.T) {
	total := 3*boundKeep + 12345
	data := make([]byte, total)
	for i := range data {
		data[i] = byte('a' + i%26)
	}
	copy(data, "HEAD-MARKER-0001")
	copy(data[total-16:], "TAIL-MARKER-9999")
	fake := &pagedRunner{data: data, page: 100000, path: "runs/bound-1/out"}
	result, err := New().Run(t.Context(), tool.Call{ID: "bound-1", Args: jsontext.Value(`{"command":"true"}`)}, tool.Env{Runner: fake})
	if err != nil {
		t.Fatal(err)
	}
	if !result.OK || result.ExitCode == nil || *result.ExitCode != 0 {
		t.Fatalf("bounded run ok = %v exit = %+v", result.OK, result.ExitCode)
	}
	requireBoundedOutput(t, string(result.Output), boundedExpectation{data: data, total: total, head: "HEAD-MARKER-0001", tail: "TAIL-MARKER-9999", path: "runs/bound-1/out"}, string(data[total-boundKeep:]))
}

type boundedExpectation struct {
	data  []byte
	total int
	head  string
	tail  string
	path  string
}

func requireBoundedOutput(t *testing.T, got string, want boundedExpectation, window string) {
	t.Helper()
	notice := "\n[output truncated: " + strconv.Itoa(want.total) + " bytes total; full output: " + want.path + "; page it with read offset/limit]"
	if !utf8.ValidString(got) {
		t.Fatal("bounded result kept part of a rune")
	}
	if !strings.HasSuffix(got, notice) {
		t.Fatalf("result misses total notice, output length = %d", len(got))
	}
	if !strings.Contains(got, strconv.Itoa(want.total)) {
		t.Fatal("bounded result misses the total byte count")
	}
	if !strings.Contains(got, want.tail) {
		t.Fatal("bounded result lost the output tail")
	}
	if strings.Contains(got, want.head) {
		t.Fatal("bounded result kept the output head")
	}
	if !strings.Contains(got, window) {
		t.Fatal("bounded result is not the intact tail window from a rune boundary")
	}
	body := strings.TrimSuffix(got, notice)
	if len(body) > boundKeep || len(body) < boundKeep-(utf8.UTFMax-1) {
		t.Fatalf("bounded result keeps %d bytes, want last 64 KiB from a rune boundary", len(body))
	}
}

func TestExecBoundsOutputTrimsPartialRune(t *testing.T) {
	total := boundKeep + 10
	data := make([]byte, total)
	for i := range data {
		data[i] = 'z'
	}
	copy(data, "HEADMARK")
	copy(data[8:], "\u20ac")
	copy(data[total-8:], "TAILMARK")
	fake := &pagedRunner{data: data, page: 20000, path: "runs/bound-2/out"}
	result, err := New().Run(t.Context(), tool.Call{ID: "bound-2", Args: jsontext.Value(`{"command":"true"}`)}, tool.Env{Runner: fake})
	if err != nil {
		t.Fatal(err)
	}
	requireBoundedOutput(t, string(result.Output), boundedExpectation{data: data, total: total, head: "HEADMARK", tail: "TAILMARK", path: "runs/bound-2/out"}, string(data[11:]))
}
