package rpc

import (
	"encoding/base64"
	json "encoding/json/v2"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/config"
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

func expectNoChange(t *testing.T, c *wireClient, window time.Duration) {
	t.Helper()
	timer := time.NewTimer(window)
	defer timer.Stop()
	select {
	case m := <-c.notes:
		t.Fatalf("unexpected %s notification %s", m.Method, m.Params)
	case <-timer.C:
	}
}

func TestFsWatchUnchangedPollAndUnwatchEndDelivery(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	dir, err := filepath.EvalSymlinks(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	file := filepath.Join(dir, "x.txt")
	if err := os.WriteFile(file, []byte("one"), 0o644); err != nil {
		t.Fatal(err)
	}
	c.ok("fs/watch", map[string]any{"watchId": "w1", "path": dir}, nil)
	if err := os.WriteFile(file, []byte("second"), 0o644); err != nil {
		t.Fatal(err)
	}
	c.await("fs/changed", func(m message) bool { return reportsChange(t, m, "w1", file) })
	expectNoChange(t, c, 1300*time.Millisecond)

	c.ok("fs/unwatch", map[string]any{"watchId": "w1"}, nil)
	if err := os.WriteFile(file, []byte("three"), 0o644); err != nil {
		t.Fatal(err)
	}
	expectNoChange(t, c, 1300*time.Millisecond)
}

func TestFsWatchRefusals(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	dir := t.TempDir()
	c.ok("fs/watch", map[string]any{"watchId": "w1", "path": dir}, nil)
	c.fails("fs/watch", map[string]any{"watchId": "w1", "path": dir}, -32602, "watch w1 exists")
	for i := 2; i <= 64; i++ {
		c.ok("fs/watch", map[string]any{"watchId": "w" + strconv.Itoa(i), "path": dir}, nil)
	}
	c.fails("fs/watch", map[string]any{"watchId": "w65", "path": dir}, -32602, "too many watches")

	crowded := t.TempDir()
	for i := range fsWatchEntryLimit + 1 {
		if err := os.WriteFile(filepath.Join(crowded, "f"+strconv.Itoa(i)), nil, 0o644); err != nil {
			t.Fatal(err)
		}
	}
	c.fails("fs/watch", map[string]any{"watchId": "w66", "path": crowded}, -32602, "directory has more than 10000 entries")
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
