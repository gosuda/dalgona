//go:build unix

package runner

import (
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"runtime/debug"
	"strings"
	"testing"
	"time"

	"golang.org/x/sys/unix"
)

const regularReadBound = 5 * time.Second

type regularReadCase struct {
	name string
	path func(t *testing.T) string
	want string
	err  error
	miss bool
}

func regularReadCases() []regularReadCase {
	return []regularReadCase{
		{"IO-03 regular file", func(t *testing.T) string { return regularReadFile(t, "bytes\n") }, "bytes\n", nil, false},
		{"IO-04 absent path", func(t *testing.T) string { return filepath.Join(t.TempDir(), "absent") }, "", fs.ErrNotExist, true},
		{"IO-04 named pipe without a writer", regularReadFifo, "", errNotRegular, false},
		{"IO-04 directory", func(t *testing.T) string { return t.TempDir() }, "", errNotRegular, false},
		{"IO-04 character device", func(*testing.T) string { return os.DevNull }, "", errNotRegular, false},
	}
}

func regularReadFile(t *testing.T, body string) string {
	t.Helper()
	path := filepath.Join(t.TempDir(), "regular.txt")
	if err := os.WriteFile(path, []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
	return path
}

func regularReadFifo(t *testing.T) string {
	t.Helper()
	path := filepath.Join(t.TempDir(), "pipe")
	if err := unix.Mkfifo(path, 0o600); err != nil {
		t.Fatal(err)
	}
	return path
}

func releaseRegularReader(path string) {
	fd, err := unix.Open(path, unix.O_WRONLY|unix.O_NONBLOCK|unix.O_CLOEXEC, 0)
	if err == nil {
		_ = unix.Close(fd)
	}
}

func regularReadWithin(t *testing.T, path string, read func() ([]byte, error)) ([]byte, error) {
	t.Helper()
	type outcome struct {
		data []byte
		err  error
	}
	done := make(chan outcome, 1)
	go func() {
		data, err := read()
		done <- outcome{data, err}
	}()
	select {
	case got := <-done:
		return got.data, got.err
	case <-time.After(regularReadBound):
		releaseRegularReader(path)
		<-done
		t.Fatalf("the read of %s did not return within %s", path, regularReadBound)
		return nil, nil
	}
}

func requireRegularRead(t *testing.T, test regularReadCase, path string, data []byte, err error) {
	t.Helper()
	if !errors.Is(err, test.err) {
		t.Fatalf("read of %s error = %v, want %v", path, err, test.err)
	}
	if errors.Is(err, fs.ErrNotExist) != test.miss {
		t.Fatalf("read of %s reports absence = %v, want %v: %v", path, !test.miss, test.miss, err)
	}
	if string(data) != test.want {
		t.Fatalf("read of %s = %q, want %q", path, data, test.want)
	}
}

func TestLocalReadFileAcceptsOnlyRegularFiles(t *testing.T) {
	local := newLocalContractRunner(t)
	for _, test := range regularReadCases() {
		t.Run(test.name, func(t *testing.T) {
			path := test.path(t)
			data, err := regularReadWithin(t, path, func() ([]byte, error) { return local.ReadFile(t.Context(), path) })
			requireRegularRead(t, test, path, data, err)
		})
	}
}

func TestLocalPagedReadAcceptsOnlyRegularFiles(t *testing.T) {
	local, err := NewLocal(filepath.Join(t.TempDir(), "runs"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = local.Close() })
	for _, test := range regularReadCases() {
		t.Run(test.name, func(t *testing.T) {
			path := test.path(t)
			data, err := regularReadWithin(t, path, func() ([]byte, error) {
				page, _, err := local.readFileAt(t.Context(), path, 0, readPageSize)
				return page, err
			})
			requireRegularRead(t, test, path, data, err)
		})
	}
}

func TestRemoteReadFileAcceptsOnlyRegularFiles(t *testing.T) {
	local, err := NewLocal(filepath.Join(t.TempDir(), "runs"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = local.Close() })
	remote := newPipedRemoteContract(t, t.Context(), local)
	for _, test := range regularReadCases() {
		t.Run(test.name, func(t *testing.T) {
			path := test.path(t)
			data, err := regularReadWithin(t, path, func() ([]byte, error) { return remote.ReadFile(t.Context(), path) })
			if errors.Is(test.err, errNotRegular) {
				if err == nil || !strings.Contains(err.Error(), errNotRegular.Error()) {
					t.Fatalf("remote read of %s error = %v, want %q", path, err, errNotRegular)
				}
				if len(data) != 0 {
					t.Fatalf("remote read of %s = %q, want no data", path, data)
				}
				return
			}
			requireRegularRead(t, test, path, data, err)
		})
	}
}

const descriptorReads = 64

func openDescriptorCount(t *testing.T) int {
	t.Helper()
	entries, err := os.ReadDir("/dev/fd")
	if err != nil {
		t.Fatal(err)
	}
	return len(entries)
}

func TestLocalReadFileReleasesTheDescriptorOfEveryLeaf(t *testing.T) {
	local := newLocalContractRunner(t)
	defer debug.SetGCPercent(debug.SetGCPercent(-1))
	for _, test := range regularReadCases() {
		t.Run(test.name, func(t *testing.T) {
			path := test.path(t)
			before := openDescriptorCount(t)
			for range descriptorReads {
				_, _ = regularReadWithin(t, path, func() ([]byte, error) { return local.ReadFile(t.Context(), path) })
			}
			if leaked := openDescriptorCount(t) - before; leaked > descriptorReads/2 {
				t.Fatalf("%d reads of %s left %d descriptors open", descriptorReads, path, leaked)
			}
		})
	}
}
