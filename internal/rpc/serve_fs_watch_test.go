package rpc

import (
	"encoding/base64"
	json "encoding/json/v2"
	"os"
	"path/filepath"
	"slices"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/config"
)

func TestFsWatchReportsChanges(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	var watched struct {
		Path string `json:"path"`
	}
	c.ok("fs/watch", map[string]any{"watchId": "w1", "path": t.TempDir()}, &watched)
	file := filepath.Join(watched.Path, "x.txt")
	c.ok("fs/writeFile", map[string]any{"path": file, "dataBase64": base64.StdEncoding.EncodeToString([]byte("hello"))}, nil)
	c.await("fs/changed", func(m message) bool { return reportsChange(t, m, "w1", file) })
	c.fails("fs/unwatch", map[string]any{"watchId": "w9"}, -32602, "unknown watch w9")
	c.ok("fs/unwatch", map[string]any{"watchId": "w1"}, nil)
}

func reportsChange(t *testing.T, m message, watchID, path string) bool {
	t.Helper()
	var note struct {
		WatchID      string   `json:"watchId"`
		ChangedPaths []string `json:"changedPaths"`
	}
	if err := json.Unmarshal(m.Params, &note); err != nil {
		t.Fatalf("decode fs/changed %s: %v", m.Params, err)
	}
	return note.WatchID == watchID && slices.Contains(note.ChangedPaths, path)
}

func TestFsWatchBudgetAcrossWatches(t *testing.T) {
	now := time.Unix(1, 0)
	budget := &fsPoller{now: func() time.Time { return now }}
	for i, want := range []bool{true, true, false} {
		if got := budget.reserve(9000); got != want {
			t.Fatalf("reserve %d of 9000 entries = %v, want %v", i, got, want)
		}
	}
	now = now.Add(fsPollInterval)
	if !budget.reserve(9000) {
		t.Fatal("reserve in the next second refused")
	}
}

func TestFsWatchDefersPollOverBudget(t *testing.T) {
	budget := &fsPoller{now: func() time.Time { return time.Unix(1, 0) }}
	if !budget.reserve(fsPollBudget) {
		t.Fatal("reserve of the whole budget refused")
	}
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "a.txt"), []byte("a"), 0o600); err != nil {
		t.Fatal(err)
	}
	watch := &fsWatcher{root: dir, isDir: true, last: map[string]fsFileState{}}
	if got := (&server{}).pollFsOnce("w1", watch, budget); got != fsSlowInterval {
		t.Fatalf("poll over the budget waited %v, want %v", got, fsSlowInterval)
	}
}
