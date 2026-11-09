package runner

import (
	"bytes"
	"encoding/base64"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"testing"
	"time"
)

func gitBinary(t *testing.T) string {
	t.Helper()
	path, err := exec.LookPath("git")
	if err != nil {
		t.Skip("git not available")
	}
	return path
}

func initRepo(t *testing.T, git string, files int, nameLen int) string {
	t.Helper()
	dir := t.TempDir()
	mustExec(t, dir, git, "init", "-q")
	for i := range files {
		name := strings.Repeat("f", nameLen) + "-" + strconv.Itoa(i)
		if err := os.WriteFile(filepath.Join(dir, name), []byte("x"), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	return dir
}

func mustExec(t *testing.T, dir, name string, args ...string) {
	t.Helper()
	cmd := exec.Command(name, args...)
	cmd.Dir = dir
	if out, err := cmd.CombinedOutput(); err != nil {
		t.Fatalf("%s %v: %v: %s", name, args, err, out)
	}
}

func TestEnumerate_skipsDeletedTrackedFile(t *testing.T) {
	git := gitBinary(t)
	dir := t.TempDir()
	mustExec(t, dir, git, "init", "-q")
	tracked := filepath.Join(dir, "gone.txt")
	if err := os.WriteFile(tracked, []byte("x"), 0o644); err != nil {
		t.Fatal(err)
	}
	mustExec(t, dir, git, "add", "gone.txt")
	if err := os.Remove(tracked); err != nil {
		t.Fatal(err)
	}
	var jobs []uploadJob
	if err := enumerate(t.Context(), dir, func(job uploadJob) error {
		jobs = append(jobs, job)
		return nil
	}); err != nil {
		t.Fatalf("enumerate error = %v, want skip of deleted file", err)
	}
	for _, job := range jobs {
		if strings.HasSuffix(job.local, "gone.txt") {
			t.Fatalf("enumerate emitted deleted file %s", job.local)
		}
	}
}

func TestPutFile_skipsVanishedFile(t *testing.T) {
	rec := &killRecorder{}
	job := uploadJob{local: filepath.Join(t.TempDir(), "vanished.txt"), remote: workspaceRoot + "/vanished.txt"}
	if err := putFile(t.Context(), rec, job); err != nil {
		t.Fatalf("putFile error = %v, want skip of vanished file", err)
	}
}

func TestEnumGit_drainsOutputOnEmitError(t *testing.T) {
	git := gitBinary(t)
	dir := initRepo(t, git, 12000, 20)
	want := errors.New("stop")
	done := make(chan error, 1)
	go func() {
		done <- enumGit(t.Context(), dir, func(uploadJob) error { return want })
	}()
	select {
	case err := <-done:
		if !errors.Is(err, want) {
			t.Fatalf("enumGit error = %v, want %v", err, want)
		}
	case <-time.After(30 * time.Second):
		t.Fatal("enumGit blocked draining git output after emit error")
	}
}

func writeRunningBinary(t *testing.T, dir string) {
	t.Helper()
	shell, err := exec.LookPath("sh")
	if err != nil {
		t.Skip("sh not available")
	}
	dest := filepath.Join(dir, "dalgona")
	blob, err := os.ReadFile(shell)
	if err != nil {
		t.Fatal(err)
	}
	if writeErr := os.WriteFile(dest, blob, 0o755); writeErr != nil {
		t.Fatal(writeErr)
	}
	running := exec.Command(dest, "-c", "sleep 30")
	if startErr := running.Start(); startErr != nil {
		t.Fatal(startErr)
	}
	t.Cleanup(func() {
		_ = running.Process.Kill()
		_, _ = running.Process.Wait()
	})
}

func runUploadScript(t *testing.T, dest string, encoded string) {
	t.Helper()
	cmd := exec.Command("sh", "-c", uploadScript, "_", dest, "0755", strconv.Itoa(len(encoded)))
	cmd.Stdin = strings.NewReader(encoded)
	out, err := cmd.CombinedOutput()
	if err != nil {
		t.Fatalf("upload script: %v: %s", err, out)
	}
}

func TestUploadScript_replacesRunningBinary(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("upload script targets POSIX sh")
	}
	dir := t.TempDir()
	dest := filepath.Join(dir, "dalgona")
	writeRunningBinary(t, dir)
	data := []byte("new-binary-content")
	encoded := base64.StdEncoding.EncodeToString(data) + "\n"
	runUploadScript(t, dest, encoded)
	got, err := os.ReadFile(dest)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(got, data) {
		t.Fatalf("installed %d bytes, want %d", len(got), len(data))
	}
	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatal(err)
	}
	if len(entries) != 1 || entries[0].Name() != "dalgona" {
		names := make([]string, 0, len(entries))
		for _, entry := range entries {
			names = append(names, entry.Name())
		}
		t.Fatalf("leftover temp files: %q", names)
	}
}
