package e2e

import (
	"os"
	"path/filepath"
	"runtime"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/auth"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/journal"
	"github.com/gosuda/dalgona/internal/dal/runner"
)

func TestStorageProbeCompatibility(t *testing.T) {
	home := t.TempDir()
	setupHome(t, home)
	verifyConfig(t, home)
	verifyAuth(t, home)
	verifyRunner(t, home)
	verifyJournal(t, home)
	verifyCopiedHome(t, home)
}

func setupHome(t *testing.T, home string) {
	t.Helper()
	if err := os.WriteFile(filepath.Join(home, "config.toml"), []byte("model = \"local/qwen-test\"\n"), 0o600); err != nil {
		t.Fatal(err)
	}
}

func verifyConfig(t *testing.T, home string) {
	t.Helper()
	t.Setenv("DAL_HOME", home)
	cfg, err := config.Load(config.ProductDal, home, config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	if cfg.Model != "local/qwen-test" {
		t.Fatalf("config model = %q", cfg.Model)
	}
}

func verifyJournal(t *testing.T, home string) {
	t.Helper()
	store, err := journal.Open(home)
	if err != nil {
		t.Fatal(err)
	}
	_, err = store.Append(t.Context(), "s1", []byte(`{"type":"seeded"}`))
	if err != nil {
		t.Fatal(err)
	}
	defer func() {
		if err := store.Close(); err != nil {
			t.Fatal(err)
		}
	}()

	verifyRebuiltMirror(t, home)
}

func verifyCopiedHome(t *testing.T, home string) {
	t.Helper()
	copied := t.TempDir()
	copyHome(t, home, copied)
	verifyCopiedConfig(t, home, copied)
	verifyCopiedJournalBytes(t, home, copied)
	verifyCopiedJournal(t, copied)
	verifyCopiedAuth(t, copied)
	verifyCopiedRunnerFiles(t, copied)
}

func verifyCopiedJournalBytes(t *testing.T, src, dst string) {
	t.Helper()
	srcPath := filepath.Join(src, "sessions", "s1", "journal.jsonl")
	dstPath := filepath.Join(dst, "sessions", "s1", "journal.jsonl")
	srcBytes, err := os.ReadFile(srcPath)
	if err != nil {
		t.Fatal(err)
	}
	dstBytes, err := os.ReadFile(dstPath)
	if err != nil {
		t.Fatal(err)
	}
	if string(srcBytes) != string(dstBytes) {
		t.Fatal("copied journal bytes differ from source")
	}
}

func verifyAuth(t *testing.T, home string) {
	t.Helper()
	cred, err := auth.Load(home)
	if err != nil {
		t.Fatal(err)
	}
	if err := cred.SetCredential("openai", auth.Credential{Access: "live"}); err != nil {
		t.Fatal(err)
	}
}

func verifyRunner(t *testing.T, home string) {
	t.Helper()
	if runtime.GOOS == "windows" {
		t.Skip("runner shim requires unix shell")
	}
	local, err := runner.NewLocal(filepath.Join(home, "runs"))
	if err != nil {
		t.Fatal(err)
	}
	defer func() {
		if err := local.Close(); err != nil {
			t.Fatal(err)
		}
	}()

	spec := runner.ProcessSpec{
		ID:      "r1",
		Argv:    []string{"sh", "-c", "exit 0"},
		Dir:     home,
		Sandbox: runner.Sandbox{Mode: runner.SandboxOff},
	}
	if err := local.Start(t.Context(), spec); err != nil {
		t.Fatal(err)
	}

	deadline := time.Now().Add(10 * time.Second)
	for {
		state, err := local.Read(t.Context(), "r1", 0)
		if err != nil {
			t.Fatal(err)
		}
		if state.Exit != nil {
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("run r1 never exited")
		}
		time.Sleep(50 * time.Millisecond)
	}
}

func copyHome(t *testing.T, src, dst string) {
	t.Helper()
	if err := os.CopyFS(dst, os.DirFS(src)); err != nil {
		t.Fatal(err)
	}
	authPath := filepath.Join(dst, "auth.json")
	if err := os.Chmod(authPath, 0o600); err != nil {
		t.Fatal(err)
	}
}

func verifyCopiedConfig(t *testing.T, src, dst string) {
	t.Helper()
	t.Setenv("DAL_HOME", dst)
	cfgSrc, err := config.Load(config.ProductDal, src, config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	cfgDst, err := config.Load(config.ProductDal, dst, config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	if cfgDst.Model != cfgSrc.Model {
		t.Fatalf("copied config model = %q, want %q", cfgDst.Model, cfgSrc.Model)
	}
}

func verifyCopiedJournal(t *testing.T, dst string) {
	t.Helper()
	store, err := journal.Open(dst)
	if err != nil {
		t.Fatal(err)
	}
	defer func() {
		closeErr := store.Close()
		if closeErr != nil {
			t.Fatal(closeErr)
		}
	}()

	second, err := store.Append(t.Context(), "s1", []byte(`{"type":"next"}`))
	if err != nil {
		t.Fatal(err)
	}
	if second != 2 {
		t.Fatalf("copied journal next id = %d, want 2", second)
	}

	var ids []uint64
	if err := store.Replay("s1", func(seq uint64, _ []byte) error {
		ids = append(ids, seq)
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	if len(ids) != 2 || ids[0] != 1 || ids[1] != 2 {
		t.Fatalf("copied replay ids = %v, want [1 2]", ids)
	}
}

func verifyCopiedAuth(t *testing.T, dst string) {
	t.Helper()
	cred, err := auth.Load(dst)
	if err != nil {
		t.Fatal(err)
	}
	found := false
	for _, entry := range cred.Entries() {
		if entry.ID == "openai" {
			found = true
		}
	}
	if !found {
		t.Fatal("copied home lost the openai credential")
	}
}

func verifyCopiedRunnerFiles(t *testing.T, dst string) {
	t.Helper()
	for _, name := range []string{"auth.json", "state.db", "sessions", "runs", "blobs", "quarantine"} {
		if _, err := os.Stat(filepath.Join(dst, name)); err != nil {
			t.Fatalf("copied home lacks %s: %v", name, err)
		}
	}
	for _, name := range []string{"out", "pid", "exit"} {
		if _, err := os.Stat(filepath.Join(dst, "runs", "r1", name)); err != nil {
			t.Fatalf("copied run lacks %s: %v", name, err)
		}
	}
}

func verifyRebuiltMirror(t *testing.T, src string) {
	t.Helper()
	renamed := t.TempDir()
	if err := os.CopyFS(renamed, os.DirFS(src)); err != nil {
		t.Fatal(err)
	}
	if err := os.Rename(filepath.Join(renamed, "state.db"), filepath.Join(renamed, "state2.db")); err != nil {
		t.Fatal(err)
	}
	store, err := journal.OpenShared(renamed)
	if err != nil {
		t.Fatal(err)
	}
	defer func() {
		if err := store.Close(); err != nil {
			t.Fatal(err)
		}
	}()

	var ids []uint64
	if err := store.Replay("s1", func(seq uint64, _ []byte) error {
		ids = append(ids, seq)
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	if len(ids) != 1 || ids[0] != 1 {
		t.Fatalf("rebuilt mirror replayed %v, want [1]", ids)
	}
}
