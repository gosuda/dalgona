package agent

import (
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/auth"
	"github.com/gosuda/dalgona/internal/dal/config"
)

func requireNoRawBytes(t *testing.T, dir, secret string) {
	t.Helper()
	for _, name := range rawBytesFiles(t, dir, secret) {
		t.Errorf("%s contains the raw secret", filepath.Join(dir, name))
	}
}

func rawBytesFiles(t *testing.T, dir, secret string) []string {
	t.Helper()
	root, err := os.OpenRoot(dir)
	if errors.Is(err, fs.ErrNotExist) {
		return nil
	}
	if err != nil {
		t.Fatal(err)
	}
	defer closeRoot(t, root)
	var found []string
	err = fs.WalkDir(root.FS(), ".", rawBytesWalker(root.FS(), secret, &found))
	if err != nil && !errors.Is(err, fs.ErrNotExist) {
		t.Fatal(err)
	}
	return found
}

func closeRoot(t *testing.T, root *os.Root) {
	t.Helper()
	if err := root.Close(); err != nil {
		t.Error(err)
	}
}

func rawBytesWalker(fsys fs.FS, secret string, found *[]string) fs.WalkDirFunc {
	return func(name string, entry fs.DirEntry, walkErr error) error {
		if walkErr != nil {
			return walkErr
		}
		if entry.IsDir() {
			return skipRunnerRuns(entry)
		}
		data, err := fs.ReadFile(fsys, name)
		if err != nil {
			return err
		}
		if strings.Contains(string(data), secret) {
			*found = append(*found, name)
		}
		return nil
	}
}

func skipRunnerRuns(entry fs.DirEntry) error {
	if entry.Name() == "runs" {
		return fs.SkipDir
	}
	return nil
}

func TestRawBytesScanFindsSecretsOutsideRuns(t *testing.T) {
	t.Parallel()
	secret := "AKIA" + strings.Repeat("Q", 16)
	dir := t.TempDir()
	files := map[string]string{
		"hit.txt":              "key " + secret,
		"nested/hit.txt":       secret,
		"clean.txt":            "plain-output",
		"runs/skipped.txt":     secret,
		"nested/runs/skip.txt": secret,
	}
	for name, body := range files {
		path := filepath.Join(dir, filepath.FromSlash(name))
		if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	got := rawBytesFiles(t, dir, secret)
	slices.Sort(got)
	if want := []string{"hit.txt", "nested/hit.txt"}; !slices.Equal(got, want) {
		t.Fatalf("files with the raw secret = %v, want %v", got, want)
	}
	if got := rawBytesFiles(t, filepath.Join(dir, "absent"), secret); len(got) != 0 {
		t.Fatalf("absent directory reports %v, want none", got)
	}
}

func TestBuiltinSecretPrintedByExecStaysOutOfJournalAndRequests(t *testing.T) {
	secret := "AKIA" + strings.Repeat("Q", 16)
	h := newHarness(t, withApproval(config.ApprovalYolo), func(h *harness) {
		store, err := auth.Load(h.home)
		if err != nil {
			t.Fatal(err)
		}
		h.cfg.Auth = store
	})
	h.model.push(
		calls(call{id: "e1", name: "exec", args: `{"command":"printf 'AKIA%s\\n' QQQQQQQQQQQQQQQQ"}`}),
		calls(call{id: "e2", name: "exec", args: `{"command":"echo plain-output"}`}),
		text("done"),
	)
	thread := h.start()
	h.submit(thread, "print the sample key and a plain line")
	h.waitIdle(thread)
	outputs := completedOutputs(h.events(thread.ID()))
	if out := outputs["e1"].Output; !strings.Contains(out, "{{secret:builtin0:0}}") || strings.Contains(out, secret) {
		t.Fatalf("secret output = %q", out)
	}
	if out := outputs["e2"].Output; !strings.Contains(out, "plain-output") || strings.Contains(out, "{{secret:") {
		t.Fatalf("plain output = %q", out)
	}
	for i := range h.model.count() {
		if strings.Contains(h.model.raw(i), secret) {
			t.Fatalf("model request %d contains the secret", i)
		}
	}
	requireNoRawBytes(t, filepath.Join(h.home, "sessions", thread.ID()), secret)
	requireNoRawBytes(t, filepath.Join(h.home, "blobs"), secret)
}
