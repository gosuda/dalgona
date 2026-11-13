package agent

import (
	"bytes"
	"context"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/runner"
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

type silentRunner struct {
	runner.Runner
	size int64
}

func (s silentRunner) Stat(context.Context, string) (runner.FileInfo, error) {
	return runner.FileInfo{Size: s.size}, nil
}

func (s silentRunner) Read(context.Context, string, int64) (runner.Output, error) {
	return runner.Output{}, nil
}

func TestSpillReadRejectsEmptyPageBelowTotal(t *testing.T) {
	thread := &Thread{id: "t", host: &Host{}, runner: silentRunner{size: 2 << 20}}
	_, _, err := thread.spillFrom(t.Context(), "call", &event.Spill{Path: "out/call", Bytes: 2 << 20}, 0)
	if err == nil || !strings.Contains(err.Error(), "no data at offset 0") {
		t.Fatalf("spillFrom error = %v, want no data at offset 0", err)
	}
}
