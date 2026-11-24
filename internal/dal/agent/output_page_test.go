package agent

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"errors"
	"io"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/runner"
)

func TestPageEndCutsBinaryAtTheByteLimit(t *testing.T) {
	chunk := bytes.Repeat([]byte{0x80}, 2*outputPageLimit)
	if got := pageEnd(chunk); got != outputPageLimit {
		t.Fatalf("pageEnd of binary output = %d, want %d", got, outputPageLimit)
	}
}

func TestPageEndTable(t *testing.T) {
	tests := []struct {
		name  string
		chunk []byte
		want  int
	}{
		{name: "short chunk is one page", chunk: []byte("abc"), want: 3},
		{name: "line break inside the window ends after it", chunk: append(append(bytes.Repeat([]byte("a"), 100), '\n'), bytes.Repeat([]byte("b"), 2*outputPageLimit)...), want: 101},
		{name: "line break at the last window byte ends the page", chunk: lineAt(outputPageLimit-1, 2*outputPageLimit), want: outputPageLimit},
		{name: "line break after the window is not used", chunk: lineAt(outputPageLimit, 2*outputPageLimit), want: outputPageLimit},
		{name: "rune split at the limit moves back to its start", chunk: runeAt(outputPageLimit-1, 2*outputPageLimit), want: outputPageLimit - 1},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			if got := pageEnd(tc.chunk); got != tc.want {
				t.Fatalf("pageEnd = %d, want %d", got, tc.want)
			}
		})
	}
}

func lineAt(index, size int) []byte {
	chunk := bytes.Repeat([]byte("a"), size)
	chunk[index] = '\n'
	return chunk
}

func runeAt(index, size int) []byte {
	chunk := bytes.Repeat([]byte("a"), size)
	copy(chunk[index:], "é")
	return chunk
}

func TestReadOutputBoundsRecordedBlobWindow(t *testing.T) {
	h := newHarness(t)
	thread := h.start()
	fixture := recordWindowSpill(t, h, thread)

	var before, after runtime.MemStats
	runtime.ReadMemStats(&before)
	page, err := thread.ReadOutput(t.Context(), fixture.callID, int64(fixture.offset))
	runtime.ReadMemStats(&after)
	requireWindowPage(t, page, err, fixture)
	if allocated := after.TotalAlloc - before.TotalAlloc; allocated > 12<<20 {
		t.Fatalf("one output page allocated %d bytes for a %d-byte spill; budget is 12 MiB", allocated, fixture.size)
	}
	requireWindowTotalOffset(t, thread, fixture.callID, fixture.size)
	requireWindowPageCanceled(t, thread, fixture.callID, fixture.offset)
}

type windowFixture struct {
	callID    string
	offset    int
	window    int
	runeStart int
	size      int
}

type expectedLegacyPage struct {
	name     string
	recorded string
	path     string
	wantErr  error
}

func recordWindowSpill(t *testing.T, h *harness, thread *Thread) windowFixture {
	t.Helper()
	const (
		size      = 33 << 20
		offset    = 11
		window    = 1 << 20
		runeStart = offset + window - 1
	)
	sha, _, err := h.journal.PutBlobStream(t.Context(), nil, func(dst io.Writer) error {
		pad := bytes.Repeat([]byte{'q'}, window)
		writeSpillPadding(t, dst, pad, runeStart)
		if _, err := io.WriteString(dst, "é"); err != nil {
			return err
		}
		writeSpillPadding(t, dst, pad, size-runeStart-len("é"))
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	recordOutputSpill(t, thread, "window", &event.Spill{Path: "blobs/" + sha, Bytes: int64(size), SHA256: sha})
	return windowFixture{callID: "window", offset: offset, window: window, runeStart: runeStart, size: size}
}

func requireWindowPage(t *testing.T, page OutputPage, err error, fixture windowFixture) {
	t.Helper()
	if err != nil || page.Offset != int64(fixture.offset) || page.Next != int64(fixture.runeStart) || page.Total != int64(fixture.size) || page.Text != strings.Repeat("q", fixture.window-1) {
		t.Fatalf("ReadOutput = %d text bytes, offset %d, next %d, total %d, error %v; want an intact page before the split rune", len(page.Text), page.Offset, page.Next, page.Total, err)
	}
}

func requireWindowTotalOffset(t *testing.T, thread *Thread, callID string, size int) {
	t.Helper()
	if _, err := thread.ReadOutput(t.Context(), callID, int64(size)); !errors.Is(err, OffsetError{Offset: int64(size), Total: int64(size)}) {
		t.Fatalf("ReadOutput at EOF = %v, want OffsetError", err)
	}
}

func requireWindowPageCanceled(t *testing.T, thread *Thread, callID string, offset int) {
	t.Helper()
	ctx, cancel := context.WithCancel(t.Context())
	cancel()
	if _, err := thread.ReadOutput(ctx, callID, int64(offset)); !errors.Is(err, context.Canceled) {
		t.Fatalf("ReadOutput with canceled context = %v, want context.Canceled", err)
	}
}

func TestReadOutputRejectsChangedBlob(t *testing.T) {
	h := newHarness(t)
	thread := h.start()
	first, err := thread.PutBlob([]byte("first"))
	if err != nil {
		t.Fatal(err)
	}
	second, err := thread.PutBlob([]byte("other"))
	if err != nil {
		t.Fatal(err)
	}
	for _, tc := range []struct {
		name string
		sha  string
		size int64
	}{
		{name: "digest", sha: second.Blob, size: first.Bytes},
		{name: "length", sha: first.Blob, size: first.Bytes + 1},
	} {
		t.Run(tc.name, func(t *testing.T) {
			recordOutputSpill(t, thread, tc.name, &event.Spill{Path: "blobs/" + first.Blob, SHA256: tc.sha, Bytes: tc.size})
			page, err := thread.ReadOutput(t.Context(), tc.name, 1)
			if !errors.Is(err, ErrSpillChanged) || page != (OutputPage{}) {
				t.Fatalf("ReadOutput = %+v, %v; want ErrSpillChanged with no output", page, err)
			}
		})
	}
}

func TestReadOutputUsesRecordedLegacyFile(t *testing.T) {
	runs := t.TempDir()
	h := newHarness(t, func(h *harness) {
		h.cfg.Runner = func(context.Context, string, string) (runner.Runner, error) {
			return runner.NewLocal(runs)
		}
	})
	thread := h.start()
	for _, tc := range []struct {
		name, recorded, file, process string
		wantErr                       error
	}{
		{name: "distinct-process", recorded: "FILE-111", file: "FILE-111", process: "CALL-111"},
		{name: "changed-file", recorded: "FILE-222", file: "DISK-222", process: "FILE-222", wantErr: ErrSpillChanged},
	} {
		t.Run(tc.name, func(t *testing.T) {
			path := writeLegacyFileFixture(t, runs, tc.name, tc.file, tc.process)
			checkRecordedLegacyPage(t, thread, expectedLegacyPage{name: tc.name, recorded: tc.recorded, path: path, wantErr: tc.wantErr})
		})
	}
}

func writeLegacyFileFixture(t *testing.T, runs, name, file, process string) string {
	t.Helper()
	path := filepath.Join(runs, name+".out")
	if err := os.WriteFile(path, []byte(file), 0o600); err != nil {
		t.Fatal(err)
	}
	run := filepath.Join(runs, name)
	if err := os.Mkdir(run, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(run, "out"), []byte(process), 0o600); err != nil {
		t.Fatal(err)
	}
	return path
}

func checkRecordedLegacyPage(t *testing.T, thread *Thread, want expectedLegacyPage) {
	t.Helper()
	total := int64(len(want.recorded))
	recordOutputSpill(t, thread, want.name, &event.Spill{Path: want.path, Bytes: total, SHA256: exactDigest([]byte(want.recorded))})
	page, err := thread.ReadOutput(t.Context(), want.name, 2)
	if want.wantErr != nil {
		if !errors.Is(err, want.wantErr) || page != (OutputPage{}) {
			t.Fatalf("ReadOutput = %+v, %v; want %v with no output", page, err, want.wantErr)
		}
		return
	}
	expected := OutputPage{Text: want.recorded[2:], Offset: 2, Next: total, Total: total}
	if err != nil || page != expected {
		t.Fatalf("ReadOutput = %+v, %v; want recorded file page %+v", page, err, expected)
	}
}

func recordOutputSpill(t *testing.T, thread *Thread, callID string, spill *event.Spill) {
	t.Helper()
	turnID := "turn-" + callID
	if err := thread.Append(t.Context(),
		event.TurnStarted{TurnID: turnID},
		event.ToolStarted{CallID: callID, TurnID: turnID, Tool: "exec", Class: "exec", Args: jsontext.Value(`{}`)},
		event.ToolCompleted{CallID: callID, OK: true, Spill: spill, Recovered: "none"},
		event.TurnCompleted{TurnID: turnID, Status: "completed"},
	); err != nil {
		t.Fatal(err)
	}
}
