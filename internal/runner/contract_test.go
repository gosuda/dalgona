//go:build !windows

package runner

import (
	"bytes"
	"context"
	"errors"
	"io"
	"io/fs"
	"net"
	"net/http"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/coder/websocket"
)

func TestRunnerContract(t *testing.T) {
	factories := []struct {
		name string
		new  func(*testing.T) Runner
	}{
		{name: "local", new: newLocalContractRunner},
		{name: "pipe", new: newPipeContractRunner},
		{name: "websocket", new: newWebsocketContractRunner},
	}
	for _, factory := range factories {
		t.Run(factory.name, func(t *testing.T) {
			r := factory.new(t)
			t.Cleanup(func() { _ = r.Close() })
			t.Run("files", func(t *testing.T) { testContractFiles(t, r) })
			t.Run("process", func(t *testing.T) { testContractProcess(t, r) })
			t.Run("signal", func(t *testing.T) { testContractSignal(t, r) })
			t.Run("missingProcess", func(t *testing.T) {
				_, err := r.Read(t.Context(), "missing", 0)
				if !errors.Is(err, ErrProcessNotFound) {
					t.Fatalf("Read error = %v, want ErrProcessNotFound", err)
				}
			})
		})
	}
}

func newLocalContractRunner(t *testing.T) Runner {
	t.Helper()
	local, err := NewLocal(filepath.Join(t.TempDir(), "runs"))
	if err != nil {
		t.Fatal(err)
	}
	return local
}

func newPipeContractRunner(t *testing.T) Runner {
	t.Helper()
	server, client := net.Pipe()
	local := newLocalContractRunner(t)
	ctx := t.Context()
	done := make(chan error, 1)
	go func() { done <- Serve(ctx, server, local.(*Local)) }()
	t.Cleanup(func() { stopContractServer(t, server, done) })
	return newRemoteContract(t, client)
}

func newWebsocketContractRunner(t *testing.T) Runner {
	t.Helper()
	local := newLocalContractRunner(t)
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	ctx := t.Context()
	done := make(chan error, 1)
	token := strings.Repeat("contract-token-", 3)
	go func() { done <- serveExecWS(ctx, listener, local.(*Local), token, nil) }()
	t.Cleanup(func() { stopContractServer(t, listener, done) })
	dialCtx, dialCancel := context.WithTimeout(t.Context(), 5*time.Second)
	defer dialCancel()
	header := http.Header{"Authorization": []string{"Bearer " + token}}
	ws, _, err := websocket.Dial(dialCtx, "ws://"+listener.Addr().String()+"/", &websocket.DialOptions{HTTPHeader: header})
	if err != nil {
		t.Fatal(err)
	}
	conn := websocket.NetConn(t.Context(), ws, websocket.MessageText)
	return newRemoteContract(t, conn)
}

func stopContractServer(t *testing.T, closer io.Closer, done <-chan error) {
	t.Helper()
	if err := closer.Close(); err != nil && !errors.Is(err, net.ErrClosed) {
		t.Error(err)
	}
	err := <-done
	expected := []error{net.ErrClosed, io.ErrClosedPipe, io.EOF, context.Canceled}
	if err != nil && !slices.ContainsFunc(expected, func(target error) bool { return errors.Is(err, target) }) {
		t.Errorf("server stopped with %v", err)
	}
}

func newRemoteContract(t *testing.T, conn io.ReadWriteCloser) Runner {
	t.Helper()
	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
	defer cancel()
	remote, err := NewRemote(ctx, "contract", conn)
	if err != nil {
		t.Fatal(err)
	}
	return remote
}

func testContractFiles(t *testing.T, r Runner) {
	t.Helper()
	dir := t.TempDir()
	path := filepath.Join(dir, "nested", "payload.bin")
	want := []byte{0, 1, 2, 0xff, '\n'}
	if err := r.WriteFile(t.Context(), path, want, 0o640); err != nil {
		t.Fatal(err)
	}
	got, err := r.ReadFile(t.Context(), path)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(got, want) {
		t.Fatalf("ReadFile = %v, want %v", got, want)
	}
	checkContractMetadata(t, r, path, int64(len(want)))
	checkContractRemove(t, r, path)
}

func checkContractMetadata(t *testing.T, r Runner, path string, size int64) {
	t.Helper()
	checkContractStat(t, r, path, size)
	checkContractReadDir(t, r, path, size)
}

func checkContractStat(t *testing.T, r Runner, path string, size int64) {
	t.Helper()
	info, err := r.Stat(t.Context(), path)
	if err != nil {
		t.Fatal(err)
	}
	if info.Name != "payload.bin" || info.Size != size || info.Mode.Perm() != 0o640 || info.ModTime.IsZero() {
		t.Fatalf("Stat = %+v", info)
	}
}

func checkContractReadDir(t *testing.T, r Runner, path string, size int64) {
	t.Helper()
	entries, err := r.ReadDir(t.Context(), filepath.Dir(path))
	if err != nil {
		t.Fatal(err)
	}
	if len(entries) != 1 || entries[0].Name != "payload.bin" || entries[0].Size != size {
		t.Fatalf("ReadDir = %+v", entries)
	}
}

func checkContractRemove(t *testing.T, r Runner, path string) {
	t.Helper()
	if err := r.Remove(t.Context(), path); err != nil {
		t.Fatal(err)
	}
	if _, err := r.Stat(t.Context(), path); !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("Stat after Remove error = %v, want fs.ErrNotExist", err)
	}
}

func testContractProcess(t *testing.T, r Runner) {
	t.Helper()
	spec := ProcessSpec{
		ID:      "process",
		Argv:    []string{"sh", "-c", "printf first; sleep 0.05; printf second"},
		Dir:     t.TempDir(),
		Sandbox: Sandbox{Mode: SandboxOff},
	}
	if err := r.Start(t.Context(), spec); err != nil {
		t.Fatal(err)
	}
	if err := r.Start(t.Context(), spec); err != nil {
		t.Fatal(err)
	}
	state := readContractOutput(t, r, spec.ID)
	if string(state.Data) != "firstsecond" {
		t.Fatalf("output = %q", state.Data)
	}
	if state.Exit == nil || state.Exit.Code != 0 || state.Alive || state.Path == "" {
		t.Fatalf("final state = %+v", state)
	}
}

func testContractSignal(t *testing.T, r Runner) {
	t.Helper()
	spec := ProcessSpec{ID: "signal", Argv: []string{"sh", "-c", "sleep 30"}, Dir: t.TempDir(), Sandbox: Sandbox{Mode: SandboxOff}}
	if err := r.Start(t.Context(), spec); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = r.Signal(context.Background(), spec.ID, SignalKill) })
	waitContractAlive(t, r, spec.ID)
	if err := r.Signal(t.Context(), spec.ID, SignalTerm); err != nil {
		t.Fatal(err)
	}
	state := readContractOutput(t, r, spec.ID)
	if state.Exit == nil || state.Alive {
		t.Fatalf("signaled state = %+v", state)
	}
}

func readContractOutput(t *testing.T, r Runner, id string) Output {
	t.Helper()
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()
	var result Output
	for {
		state, err := r.Read(ctx, id, result.Next)
		if err != nil {
			t.Fatal(err)
		}
		result.Data = append(result.Data, state.Data...)
		result.Next = state.Next
		result.Exit = state.Exit
		result.Alive = state.Alive
		result.Path = state.Path
		if state.Exit != nil {
			return result
		}
		select {
		case <-ctx.Done():
			t.Fatal(ctx.Err())
		case <-time.After(10 * time.Millisecond):
		}
	}
}

func waitContractAlive(t *testing.T, r Runner, id string) {
	t.Helper()
	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
	defer cancel()
	for {
		state, err := r.Read(ctx, id, 0)
		if err != nil {
			t.Fatal(err)
		}
		if state.Alive {
			return
		}
		select {
		case <-ctx.Done():
			t.Fatal(ctx.Err())
		case <-time.After(10 * time.Millisecond):
		}
	}
}

func newPipedRemoteContract(t *testing.T, ctx context.Context, local *Local) Runner {
	t.Helper()
	server, client := net.Pipe()
	done := make(chan error, 1)
	go func() { done <- Serve(ctx, server, local) }()
	t.Cleanup(func() { stopContractServer(t, server, done) })
	remote := newRemoteContract(t, client)
	t.Cleanup(func() { _ = remote.Close() })
	return remote
}

func pipeFileRoundTrip(t *testing.T, ctx context.Context, newPiped func(*testing.T) Runner, local *Local, want []byte) {
	t.Helper()
	path := filepath.Join(t.TempDir(), "large.bin")
	if err := local.WriteFile(ctx, path, want, 0o600); err != nil {
		t.Fatal(err)
	}
	got, err := newPiped(t).ReadFile(ctx, path)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(got, want) {
		t.Fatalf("ReadFile returned %d bytes, want %d", len(got), len(want))
	}
	upload := filepath.Join(t.TempDir(), "upload.bin")
	if err := newPiped(t).WriteFile(ctx, upload, want, 0o600); err != nil {
		t.Fatal(err)
	}
	stored, readErr := local.ReadFile(ctx, upload)
	if readErr != nil {
		t.Fatal(readErr)
	}
	if !bytes.Equal(stored, want) {
		t.Fatalf("WriteFile stored %d bytes, want %d", len(stored), len(want))
	}
}

func TestRemotePagesLargePayloads(t *testing.T) {
	ctx := t.Context()
	local, ok := newLocalContractRunner(t).(*Local)
	if !ok {
		t.Fatal("local contract runner is not *Local")
	}
	newPiped := func(t *testing.T) Runner {
		t.Helper()
		return newPipedRemoteContract(t, ctx, local)
	}
	const size = 13 << 20
	want := bytes.Repeat([]byte("abcdefg1"), size/8)
	t.Run("file", func(t *testing.T) {
		pipeFileRoundTrip(t, ctx, newPiped, local, want)
	})
	t.Run("processRead", func(t *testing.T) {
		spec := ProcessSpec{
			ID:      "large-output",
			Argv:    []string{"sh", "-c", "head -c 13631488 /dev/zero | tr '\\0' 'p'"},
			Dir:     t.TempDir(),
			Sandbox: Sandbox{Mode: SandboxOff},
		}
		remote := newPiped(t)
		if err := remote.Start(ctx, spec); err != nil {
			t.Fatal(err)
		}
		state := drainRemoteProcess(t, remote, ctx, spec.ID)
		if len(state.Data) != size || !bytes.Equal(state.Data, bytes.Repeat([]byte("p"), size)) {
			t.Fatalf("process output = %d bytes, want %d of 'p'", len(state.Data), size)
		}
	})
}

func drainRemoteProcess(t *testing.T, remote Runner, ctx context.Context, id string) Output {
	t.Helper()
	drainCtx, cancel := context.WithTimeout(ctx, 120*time.Second)
	defer cancel()
	var state Output
	for {
		page, err := remote.Read(drainCtx, id, state.Next)
		if err != nil {
			t.Fatal(err)
		}
		state.Data = append(state.Data, page.Data...)
		state.Next = page.Next
		if page.Exit != nil {
			return state
		}
		if drainCtx.Err() != nil {
			t.Fatal(drainCtx.Err())
		}
	}
}
