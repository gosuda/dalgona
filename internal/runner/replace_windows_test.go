//go:build windows

package runner

import (
	"bytes"
	"errors"
	"os"
	"path/filepath"
	"syscall"
	"testing"
	"time"

	"golang.org/x/sys/windows"
)

func writeReplaceTarget(t *testing.T, dir string, data []byte) string {
	t.Helper()
	path := filepath.Join(dir, "pid")
	if err := os.WriteFile(path, data, 0o600); err != nil {
		t.Fatal(err)
	}
	return path
}

func stageReplaceTemp(t *testing.T, dir string, data []byte) string {
	t.Helper()
	file, err := os.CreateTemp(dir, ".dalgona-test-")
	if err != nil {
		t.Fatal(err)
	}
	name := file.Name()
	t.Cleanup(func() { _ = os.Remove(name) })
	if _, err := file.Write(data); err != nil {
		_ = file.Close()
		t.Fatal(err)
	}
	if err := file.Close(); err != nil {
		t.Fatal(err)
	}
	return name
}

func holdReplaceOpen(t *testing.T, path string) *os.File {
	t.Helper()
	held, err := os.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	return held
}

func releaseReplaceAfter(held *os.File, delay time.Duration) <-chan struct{} {
	done := make(chan struct{})
	go func() {
		defer close(done)
		time.Sleep(delay)
		_ = held.Close()
	}()
	return done
}

func readReplaceTarget(t *testing.T, path string) []byte {
	t.Helper()
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	return data
}

func TestPublishReplaceSkipsIdenticalHeldFile(t *testing.T) {
	dir := t.TempDir()
	data := []byte("48151623 1357924680\n")
	dst := writeReplaceTarget(t, dir, data)
	held := holdReplaceOpen(t, dst)
	done := releaseReplaceAfter(held, 100*time.Millisecond)
	tmp := stageReplaceTemp(t, dir, data)
	if err := publishReplace(tmp, dst, 0o600); err != nil {
		t.Fatalf("publish identical held file: %v", err)
	}
	<-done
	if actual := readReplaceTarget(t, dst); !bytes.Equal(actual, data) {
		t.Fatalf("held file content %q, want %q", actual, data)
	}
}

func TestPublishReplaceRetriesHeldFile(t *testing.T) {
	dir := t.TempDir()
	dst := writeReplaceTarget(t, dir, []byte("old content\n"))
	held := holdReplaceOpen(t, dst)
	done := releaseReplaceAfter(held, 100*time.Millisecond)
	next := []byte("new content\n")
	tmp := stageReplaceTemp(t, dir, next)
	if err := publishReplace(tmp, dst, 0o600); err != nil {
		t.Fatalf("publish held file: %v", err)
	}
	<-done
	if actual := readReplaceTarget(t, dst); !bytes.Equal(actual, next) {
		t.Fatalf("replaced content %q, want %q", actual, next)
	}
}

func TestPublishReplaceHeldPastDeadlineKeepsOriginalError(t *testing.T) {
	dir := t.TempDir()
	before := []byte("old content\n")
	dst := writeReplaceTarget(t, dir, before)
	held := holdReplaceOpen(t, dst)
	defer func() { _ = held.Close() }()
	tmp := stageReplaceTemp(t, dir, []byte("new content\n"))
	err := publishReplace(tmp, dst, 0o600)
	if err == nil {
		t.Fatal("publish held file succeeded, want access denied or sharing violation")
	}
	errno, ok := errors.AsType[syscall.Errno](err)
	if !ok {
		t.Fatalf("publish held file error type %T, want errno", err)
	}
	if errno != syscall.ERROR_ACCESS_DENIED && errno != windows.ERROR_SHARING_VIOLATION {
		t.Fatalf("publish held file errno %d, want access denied or sharing violation", errno)
	}
	if actual := readReplaceTarget(t, dst); !bytes.Equal(actual, before) {
		t.Fatalf("held file content %q, want %q", actual, before)
	}
}

func TestPublishReplaceDirectoryFailsFast(t *testing.T) {
	dir := t.TempDir()
	dst := filepath.Join(dir, "subdir")
	if err := os.Mkdir(dst, 0o700); err != nil {
		t.Fatal(err)
	}
	tmp := stageReplaceTemp(t, dir, []byte("new content\n"))
	start := time.Now()
	err := publishReplace(tmp, dst, 0o600)
	elapsed := time.Since(start)
	if err == nil {
		t.Fatal("publish onto directory succeeded, want error")
	}
	if elapsed >= replaceRetryDeadline/2 {
		t.Fatalf("publish onto directory took %v, want well under %v", elapsed, replaceRetryDeadline)
	}
}

func TestPublishReplaceReplacesSymlinkDestination(t *testing.T) {
	dir := t.TempDir()
	data := []byte("linked content\n")
	target := filepath.Join(dir, "target.txt")
	if writeErr := os.WriteFile(target, data, 0o600); writeErr != nil {
		t.Fatal(writeErr)
	}
	link := filepath.Join(dir, "link.txt")
	if linkErr := os.Symlink(target, link); linkErr != nil {
		t.Skipf("cannot create links: %v", linkErr)
	}
	tmp := stageReplaceTemp(t, dir, data)
	if err := publishReplace(tmp, link, 0o600); err != nil {
		t.Fatalf("publish over symlink: %v", err)
	}
	info, statErr := os.Lstat(link)
	if statErr != nil {
		t.Fatal(statErr)
	}
	if info.Mode()&os.ModeSymlink != 0 {
		t.Fatal("publish kept the symlink, want a regular file")
	}
	if actual := readReplaceTarget(t, link); !bytes.Equal(actual, data) {
		t.Fatalf("replaced link content %q, want %q", actual, data)
	}
}

func TestPublishReplaceRenamesLargeIdenticalFile(t *testing.T) {
	dir := t.TempDir()
	data := bytes.Repeat([]byte("x"), replaceKeepLimit+1)
	dst := writeReplaceTarget(t, dir, data)
	held := holdReplaceOpen(t, dst)
	defer func() { _ = held.Close() }()
	tmp := stageReplaceTemp(t, dir, data)
	if err := publishReplace(tmp, dst, 0o600); err == nil {
		t.Fatal("publish held large file succeeded, want bounded retry error")
	}
}
