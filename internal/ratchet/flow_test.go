package ratchet

import (
	"crypto/sha256"
	"encoding/hex"
	"os"
	"path/filepath"
	"testing"
	"time"
)

func newWorkspace(t *testing.T, files map[string]string) *flow {
	t.Helper()
	dir := t.TempDir()
	for name, body := range files {
		p := filepath.Join(dir, filepath.FromSlash(name))
		if err := os.MkdirAll(filepath.Dir(p), 0o750); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(p, []byte(body), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	f, err := openFlow(dir, "live")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = f.close() })
	return f
}

func sumOf(text string) string {
	sum := sha256.Sum256([]byte(text))
	return hex.EncodeToString(sum[:])
}

func TestLoadMissingFlowSaysInit(t *testing.T) {
	t.Parallel()
	f := newWorkspace(t, nil)
	if _, err := f.load(); err == nil || err.Error() != `flow "live" does not exist; call init first` {
		t.Fatalf("load before save = %v, want the missing flow error", err)
	}
}

func TestFlowSaveLoadRoundTrip(t *testing.T) {
	t.Parallel()
	f := newWorkspace(t, nil)
	at := time.Date(2026, 11, 11, 7, 0, 0, 0, time.UTC)
	in := &state{
		Flow:       "live",
		CasesPaths: []string{"a.txt"},
		Goal:       goal{Target: "score", Direction: "higher"},
		Reps:       2,
		Stop:       stopRule{Plateau: 3, Rounds: 10},
		Approvals:  map[string]approval{"inputs": {SHA: "ab", At: at}},
	}
	if err := f.save(in); err != nil {
		t.Fatal(err)
	}
	out, err := f.load()
	if err != nil {
		t.Fatal(err)
	}
	checkLoaded(t, out, at)
	if _, err := f.root.Stat(filepath.Join(f.dir, stateFile+".tmp")); !os.IsNotExist(err) {
		t.Fatalf("temporary state file still exists: %v", err)
	}
}

func checkLoaded(t *testing.T, out *state, at time.Time) {
	t.Helper()
	if out.Flow != "live" || out.Goal.Target != "score" || out.Reps != 2 || out.Stop.Rounds != 10 {
		t.Fatalf("loaded state = %+v, want the saved values", out)
	}
	if got := out.Approvals["inputs"]; got.SHA != "ab" || !got.At.Equal(at) {
		t.Fatalf("loaded approval = %+v, want sha ab at %s", got, at)
	}
}
