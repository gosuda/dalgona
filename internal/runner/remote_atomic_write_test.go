//go:build !windows

package runner

import (
	"bufio"
	"bytes"
	"context"
	"net"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"
)

type scriptedFault struct {
	failAt int
	writes int
}

func (f *scriptedFault) inject() bool {
	f.writes++
	return f.failAt > 0 && f.writes == f.failAt
}

func newScriptedRemote(t *testing.T, local *Local, failWriteAt int) *Remote {
	t.Helper()
	server, client := net.Pipe()
	done := make(chan struct{})
	fault := &scriptedFault{failAt: failWriteAt}
	go func() {
		defer close(done)
		defer func() { _ = server.Close() }()
		reader := bufio.NewReader(server)
		writer := rpcWriter{w: server, mu: &sync.Mutex{}}
		ctx := context.Background()
		for {
			line, readErr := readLimitedLine(reader, maxRPCLine)
			if readErr != nil {
				return
			}
			msg, parseErr := parseRPC(line)
			if parseErr != nil {
				_ = writeRPCError(writer, nil, rpcParseError, "parse error")
				continue
			}
			if replyErr := scriptedReply(ctx, local, writer, fault, msg); replyErr != nil {
				return
			}
		}
	}()
	t.Cleanup(func() {
		_ = client.Close()
		<-done
	})
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()
	remote, err := NewRemote(ctx, "atomic-test", client)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = remote.Close() })
	return remote
}

func scriptedReply(ctx context.Context, local *Local, writer rpcWriter, fault *scriptedFault, msg rpcMessage) error {
	if msg.method == "" {
		return nil
	}
	if msg.method == "fs/writeFile" && fault.inject() {
		return writeRPCError(writer, msg.id, rpcInternal, "injected chunk failure")
	}
	result, callErr := callMethod(ctx, local, msg.method, msg.params, trustedExecPolicy())
	if !msg.hasID {
		return nil
	}
	if callErr != nil {
		code, text := rpcStatus(callErr)
		return writeRPCError(writer, msg.id, code, text)
	}
	return writeRPCResult(writer, msg.id, result)
}

func TestRemoteWriteFileFailureKeepsDestination(t *testing.T) {
	dir := t.TempDir()
	local, err := NewLocal(filepath.Join(dir, "runs"))
	if err != nil {
		t.Fatal(err)
	}
	remote := newScriptedRemote(t, local, 2)
	dest := filepath.Join(dir, "target.txt")
	if writeErr := os.WriteFile(dest, []byte("original-bytes"), 0o600); writeErr != nil {
		t.Fatal(writeErr)
	}
	data := bytes.Repeat([]byte("n"), 2*readPageSize+16)
	ctx, cancel := context.WithTimeout(t.Context(), 30*time.Second)
	defer cancel()
	if uploadErr := remote.WriteFile(ctx, dest, data, 0o600); uploadErr == nil {
		t.Fatal("WriteFile succeeded after an injected chunk failure")
	}
	got, readErr := os.ReadFile(dest)
	if readErr != nil {
		t.Fatal(readErr)
	}
	if string(got) != "original-bytes" {
		t.Fatalf("destination holds %d bytes after a failed upload, want the original 14 bytes", len(got))
	}
	assertNoStagedFiles(t, dir)
}

func TestRemoteWriteFilePublishesStagedUpload(t *testing.T) {
	dir := t.TempDir()
	local, err := NewLocal(filepath.Join(dir, "runs"))
	if err != nil {
		t.Fatal(err)
	}
	remote := newScriptedRemote(t, local, 0)
	dest := filepath.Join(dir, "nested", "payload.bin")
	want := make([]byte, 2*readPageSize+16)
	for i := range want {
		want[i] = byte(i % 251)
	}
	ctx, cancel := context.WithTimeout(t.Context(), 30*time.Second)
	defer cancel()
	if uploadErr := remote.WriteFile(ctx, dest, want, 0o640); uploadErr != nil {
		t.Fatal(uploadErr)
	}
	got, err := remote.ReadFile(ctx, dest)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(got, want) {
		t.Fatalf("staged upload holds %d bytes, want %d", len(got), len(want))
	}
	info, err := remote.Stat(ctx, dest)
	if err != nil {
		t.Fatal(err)
	}
	if info.Mode.Perm() != 0o640 {
		t.Fatalf("staged upload mode = %o, want 640", info.Mode.Perm())
	}
	assertNoStagedFiles(t, dir)
}

func assertNoStagedFiles(t *testing.T, dir string) {
	t.Helper()
	staged, err := collectStaged(dir)
	if err != nil {
		t.Fatal(err)
	}
	if len(staged) > 0 {
		t.Fatalf("staged files remain: %q", staged)
	}
}

func collectStaged(dir string) ([]string, error) {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil, err
	}
	var staged []string
	for _, entry := range entries {
		if strings.Contains(entry.Name(), ".dalgona-partial-") {
			staged = append(staged, entry.Name())
			continue
		}
		if !entry.IsDir() {
			continue
		}
		nested, nestedErr := stagedInDir(dir, entry.Name())
		if nestedErr != nil {
			return nil, nestedErr
		}
		staged = append(staged, nested...)
	}
	return staged, nil
}

func stagedInDir(dir, name string) ([]string, error) {
	entries, err := os.ReadDir(filepath.Join(dir, name))
	if err != nil {
		return nil, err
	}
	var staged []string
	for _, entry := range entries {
		if strings.Contains(entry.Name(), ".dalgona-partial-") {
			staged = append(staged, filepath.Join(name, entry.Name()))
		}
	}
	return staged, nil
}
