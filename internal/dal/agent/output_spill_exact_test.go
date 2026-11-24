package agent

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/runner"
)

type exactFileRunner struct {
	runner.Runner
	files map[string][]byte
}

func (f exactFileRunner) Stat(_ context.Context, path string) (runner.FileInfo, error) {
	data, ok := f.files[path]
	if !ok {
		return runner.FileInfo{}, fs.ErrNotExist
	}
	return runner.FileInfo{Name: path, Size: int64(len(data))}, nil
}

func (f exactFileRunner) Read(_ context.Context, id string, offset int64) (runner.Output, error) {
	data := f.files[id]
	if offset > int64(len(data)) {
		return runner.Output{}, nil
	}
	return runner.Output{Data: data[offset:], Next: int64(len(data))}, nil
}

func (f exactFileRunner) ReadFilePage(_ context.Context, path string, offset, limit int64) ([]byte, int64, error) {
	data, ok := f.files[path]
	if !ok {
		return nil, 0, fs.ErrNotExist
	}
	end := min(offset+limit, int64(len(data)))
	return data[offset:end], end, nil
}

func exactDigest(data []byte) string {
	sum := sha256.Sum256(data)
	return hex.EncodeToString(sum[:])
}

func exactThread(h *harness, files map[string][]byte) *Thread {
	return &Thread{id: "exact", host: h.host, runner: exactFileRunner{files: files}}
}

func TestReadSpillExactKeepsFirstTurnBytes(t *testing.T) {
	h := newHarness(t)
	first := []byte("FIRST-01")
	second := []byte("SECOND-2")
	files := map[string][]byte{"spill/first": first, "spill/second": second, "c1": second}
	thread := exactThread(h, files)
	spill := &event.Spill{Path: "spill/first", Bytes: int64(len(first)), SHA256: exactDigest(first)}
	got, total, err := thread.ReadSpillExact(t.Context(), spill, 0, int64(len(first)))
	if err != nil || total != int64(len(first)) || string(got) != string(first) {
		t.Fatalf("ReadSpillExact = %q, %d, %v; want %q", got, total, err, first)
	}
	delete(files, "spill/first")
	part, _, err := thread.ReadSpillExact(t.Context(), spill, 2, 3)
	if err != nil || string(part) != string(first[2:5]) {
		t.Fatalf("ReadSpillExact window = %q, %v; want %q", part, err, first[2:5])
	}
	empty, total, err := thread.ReadSpillExact(t.Context(), spill, int64(len(first)), 8)
	if err != nil || len(empty) != 0 || total != int64(len(first)) {
		t.Fatalf("ReadSpillExact at end = %q, %d, %v; want empty", empty, total, err)
	}
}

func TestReadSpillExactRejectsRunnerDrift(t *testing.T) {
	h := newHarness(t)
	first := []byte("FIRST-01")
	files := map[string][]byte{"spill/first": first}
	thread := exactThread(h, files)
	spill := &event.Spill{Path: "spill/first", Bytes: int64(len(first)), SHA256: exactDigest(first)}
	files["spill/first"] = []byte("FIRST-012")
	if _, _, err := thread.ReadSpillExact(t.Context(), spill, 0, 8); !errors.Is(err, ErrSpillChanged) {
		t.Fatalf("grown spill error = %v, want %v", err, ErrSpillChanged)
	}
	if _, _, err := thread.ReadSpillExact(t.Context(), spill, 4, 4); !errors.Is(err, ErrSpillChanged) {
		t.Fatalf("grown spill window error = %v, want %v", err, ErrSpillChanged)
	}
	files["spill/first"] = []byte("FIRST-02")
	if _, _, err := thread.ReadSpillExact(t.Context(), spill, 0, 8); !errors.Is(err, ErrSpillChanged) {
		t.Fatalf("swapped spill error = %v, want %v", err, ErrSpillChanged)
	}
	if _, _, err := thread.ReadSpillExact(t.Context(), spill, 0, 2); !errors.Is(err, ErrSpillChanged) {
		t.Fatalf("swapped spill window error = %v, want %v", err, ErrSpillChanged)
	}
}

func TestReadSpillExactRunnerBoundaries(t *testing.T) {
	h := newHarness(t)
	first := []byte("FIRST-01")
	thread := exactThread(h, map[string][]byte{"spill/first": first})
	spill := &event.Spill{Path: "spill/first", Bytes: int64(len(first)), SHA256: exactDigest(first)}
	_, _, err := thread.ReadSpillExact(t.Context(), spill, int64(len(first))+1, 1)
	if _, ok := errors.AsType[OffsetError](err); !ok {
		t.Fatalf("past-end error = %v, want OffsetError", err)
	}
	_, _, err = thread.ReadSpillExact(t.Context(), spill, -1, 1)
	if _, ok := errors.AsType[OffsetError](err); !ok {
		t.Fatalf("negative error = %v, want OffsetError", err)
	}
	if _, _, err := thread.ReadSpillExact(t.Context(), nil, 0, 1); !errors.Is(err, ErrNoSpill) {
		t.Fatalf("nil spill error = %v, want %v", err, ErrNoSpill)
	}
	if _, _, err := thread.ReadSpillExact(t.Context(), &event.Spill{Path: "spill/missing", Bytes: 8, SHA256: exactDigest(first)}, 0, 8); err == nil {
		t.Fatal("missing spill read succeeded, want an error")
	}
}

func TestReadSpillExactBlob(t *testing.T) {
	h := newHarness(t)
	data := []byte("blob-exact-1234")
	sha, err := h.journal.PutBlob(data)
	if err != nil {
		t.Fatal(err)
	}
	thread := exactThread(h, nil)
	spill := &event.Spill{Path: "blobs/" + sha, Bytes: int64(len(data)), SHA256: sha}
	t.Run("full", func(t *testing.T) {
		checkSpillExactFull(t, thread, spill, data)
	})
	t.Run("window", func(t *testing.T) {
		checkSpillExactWindow(t, thread, spill, data)
	})
	t.Run("atEnd", func(t *testing.T) {
		checkSpillExactAtEnd(t, thread, spill, data)
	})
	t.Run("digestMismatch", func(t *testing.T) {
		checkSpillExactDigest(t, thread, spill, data)
	})
	t.Run("sizeMismatch", func(t *testing.T) {
		checkSpillExactSize(t, thread, spill, data)
	})
	t.Run("tampered", func(t *testing.T) {
		checkSpillExactTampered(t, h, thread, spill, data)
	})
}

func checkSpillExactFull(t *testing.T, thread *Thread, spill *event.Spill, data []byte) {
	t.Helper()
	got, total, err := thread.ReadSpillExact(t.Context(), spill, 0, int64(len(data)))
	if err != nil || total != int64(len(data)) || string(got) != string(data) {
		t.Fatalf("ReadSpillExact blob = %q, %d, %v; want %q", got, total, err, data)
	}
}

func checkSpillExactWindow(t *testing.T, thread *Thread, spill *event.Spill, data []byte) {
	t.Helper()
	part, _, err := thread.ReadSpillExact(t.Context(), spill, 5, 5)
	if err != nil || string(part) != string(data[5:10]) {
		t.Fatalf("ReadSpillExact blob window = %q, %v; want %q", part, err, data[5:10])
	}
}

func checkSpillExactAtEnd(t *testing.T, thread *Thread, spill *event.Spill, data []byte) {
	t.Helper()
	empty, _, err := thread.ReadSpillExact(t.Context(), spill, int64(len(data)), 8)
	if err != nil || len(empty) != 0 {
		t.Fatalf("ReadSpillExact blob at end = %q, %v; want empty", empty, err)
	}
}

func checkSpillExactDigest(t *testing.T, thread *Thread, spill *event.Spill, data []byte) {
	t.Helper()
	renamed := &event.Spill{Path: spill.Path, Bytes: int64(len(data)), SHA256: exactDigest([]byte("other"))}
	if _, _, err := thread.ReadSpillExact(t.Context(), renamed, 0, 1); !errors.Is(err, ErrSpillChanged) {
		t.Fatalf("digest mismatch error = %v, want %v", err, ErrSpillChanged)
	}
}

func checkSpillExactSize(t *testing.T, thread *Thread, spill *event.Spill, data []byte) {
	t.Helper()
	resized := &event.Spill{Path: spill.Path, Bytes: int64(len(data)) + 1, SHA256: spill.SHA256}
	if _, _, err := thread.ReadSpillExact(t.Context(), resized, 0, 1); !errors.Is(err, ErrSpillChanged) {
		t.Fatalf("size mismatch error = %v, want %v", err, ErrSpillChanged)
	}
}

func checkSpillExactTampered(t *testing.T, h *harness, thread *Thread, spill *event.Spill, data []byte) {
	t.Helper()
	tampered := append([]byte(nil), data...)
	tampered[len(tampered)-1] ^= 0x01
	if err := os.WriteFile(filepath.Join(h.home, "blobs", spill.SHA256), tampered, 0o600); err != nil {
		t.Fatal(err)
	}
	if _, _, err := thread.ReadSpillExact(t.Context(), spill, 0, int64(len(data))); !errors.Is(err, ErrSpillChanged) {
		t.Fatalf("tampered blob error = %v, want %v", err, ErrSpillChanged)
	}
}
