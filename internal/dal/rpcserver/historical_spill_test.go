package rpcserver

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json/jsontext"
	"errors"
	"os"
	"path/filepath"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func historicalDigest(data []byte) string {
	sum := sha256.Sum256(data)
	return hex.EncodeToString(sum[:])
}

func historicalThread(t *testing.T, r *rig, first, second []byte) (string, *rpc.OutputSpill) {
	t.Helper()
	dir := t.TempDir()
	firstPath := filepath.Join(dir, "first.out")
	secondPath := filepath.Join(dir, "second.out")
	if err := os.WriteFile(firstPath, first, 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(secondPath, second, 0o600); err != nil {
		t.Fatal(err)
	}
	thread, err := r.host.Start(t.Context(), agent.ThreadSpec{})
	if err != nil {
		t.Fatal(err)
	}
	appendTurn := func(turnID string, spill *event.Spill, output string) {
		t.Helper()
		err := thread.Append(t.Context(),
			event.TurnStarted{TurnID: turnID},
			event.ToolStarted{CallID: "c1", TurnID: turnID, Tool: "exec", Class: "exec", Args: jsontext.Value(`{}`)},
			event.ToolCompleted{CallID: "c1", OK: true, Output: output, Spill: spill, Recovered: "none"},
			event.TurnCompleted{TurnID: turnID, Status: "completed"},
		)
		if err != nil {
			t.Fatal(err)
		}
	}
	appendTurn("t1", &event.Spill{Path: firstPath, Bytes: int64(len(first)), SHA256: historicalDigest(first)}, string(first))
	appendTurn("t2", &event.Spill{Path: secondPath, Bytes: int64(len(second)), SHA256: historicalDigest(second)}, string(second))
	return thread.ID(), &rpc.OutputSpill{Path: firstPath, Bytes: int64(len(first)), SHA256: historicalDigest(first)}
}

func TestSpillRangeKeepsFirstTurnBytes(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	first := []byte("FIRST-01")
	second := []byte("SECOND-2")
	threadID, firstSpill := historicalThread(t, r, first, second)
	s := &server{host: r.host}
	got, err := s.spillRange(t.Context(), threadID, firstSpill, byteRange{offset: 0, limit: 8})
	if err != nil || string(got) != string(first) {
		t.Fatalf("spillRange first turn = %q, %v; want %q", got, err, first)
	}
	part, err := s.spillRange(t.Context(), threadID, firstSpill, byteRange{offset: 2, limit: 3})
	if err != nil || string(part) != string(first[2:5]) {
		t.Fatalf("spillRange first turn window = %q, %v; want %q", part, err, first[2:5])
	}
	empty, err := s.spillRange(t.Context(), threadID, firstSpill, byteRange{offset: 8, limit: 8})
	if err != nil || len(empty) != 0 {
		t.Fatalf("spillRange at end = %q, %v; want empty", empty, err)
	}
}

func TestSpillRangeRejectsChangedSpill(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	first := []byte("FIRST-01")
	second := []byte("SECOND-2")
	threadID, firstSpill := historicalThread(t, r, first, second)
	s := &server{host: r.host}
	if err := os.WriteFile(firstSpill.Path, []byte("FIRST-012"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := s.spillRange(t.Context(), threadID, firstSpill, byteRange{offset: 0, limit: 8}); !errors.Is(err, errContentDrift) {
		t.Fatalf("grown spill error = %v, want content drift", err)
	}
	if _, err := s.spillRange(t.Context(), threadID, firstSpill, byteRange{offset: 4, limit: 4}); !errors.Is(err, errContentDrift) {
		t.Fatalf("grown spill window error = %v, want content drift", err)
	}
	if err := os.WriteFile(firstSpill.Path, []byte("FIRST-02"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := s.spillRange(t.Context(), threadID, firstSpill, byteRange{offset: 0, limit: 8}); !errors.Is(err, errContentDrift) {
		t.Fatalf("swapped spill error = %v, want content drift", err)
	}
	if _, err := s.spillRange(t.Context(), threadID, firstSpill, byteRange{offset: 0, limit: 2}); !errors.Is(err, errContentDrift) {
		t.Fatalf("swapped spill window error = %v, want content drift", err)
	}
}

func TestSpillRangeBlobDescriptorMismatchDrifts(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	data := []byte("blob-historical")
	sha, err := r.cfg.Journal.PutBlob(data)
	if err != nil {
		t.Fatal(err)
	}
	thread, err := r.host.Start(t.Context(), agent.ThreadSpec{})
	if err != nil {
		t.Fatal(err)
	}
	s := &server{host: r.host}
	renamed := &rpc.OutputSpill{Path: "blobs/" + sha, Bytes: int64(len(data)), SHA256: historicalDigest([]byte("other"))}
	if _, err := s.spillRange(t.Context(), thread.ID(), renamed, byteRange{offset: 0, limit: 4}); !errors.Is(err, errContentDrift) {
		t.Fatalf("digest mismatch error = %v, want content drift", err)
	}
}
