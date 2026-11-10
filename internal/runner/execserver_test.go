//go:build !windows

package runner

import (
	"bytes"
	"context"
	"crypto/rand"
	"crypto/sha256"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"net"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"sync"
	"syscall"
	"testing"
	"time"

	"github.com/coder/websocket"
)

func TestExecServerMain_requiresWebsocketToken(t *testing.T) {
	t.Setenv(boxEnvironmentKey, "")
	var stdout bytes.Buffer
	code := ExecServerMain([]string{"--listen", "ws://127.0.0.1:0", "--root", t.TempDir()}, strings.NewReader(""), &stdout)
	if code == 0 {
		t.Fatal("ExecServerMain succeeded without a token")
	}
	want := "exec-server: ws listen needs DALGONA_BOX_TOKEN (32+ characters)\n"
	if stdout.String() != want {
		t.Fatalf("diagnostic = %q, want %q", stdout.String(), want)
	}
}

func TestExecWS_rejectsWrongToken(t *testing.T) {
	local := newLocalContractRunner(t)
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	done := make(chan error, 1)
	go func() { done <- serveExecWS(t.Context(), listener, local.(*Local), strings.Repeat("right-", 8), nil) }()
	t.Cleanup(func() { stopContractServer(t, listener, done) })
	header := http.Header{"Authorization": []string{"Bearer " + strings.Repeat("wrong-", 8)}}
	_, response, err := websocket.Dial(t.Context(), "ws://"+listener.Addr().String()+"/", &websocket.DialOptions{HTTPHeader: header})
	if err == nil {
		t.Fatal("websocket Dial succeeded with a wrong token")
	}
	if response == nil || response.StatusCode != http.StatusUnauthorized {
		t.Fatalf("response = %+v, error = %v", response, err)
	}
}

func TestExecWS_bridge(t *testing.T) {
	local := newLocalContractRunner(t)
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	token := strings.Repeat("bridge-token-", 3)
	bridge := func(_ context.Context, conn io.ReadWriteCloser) error {
		return writeFull(conn, []byte("bridge-ok"))
	}
	done := make(chan error, 1)
	go func() { done <- serveExecWS(t.Context(), listener, local.(*Local), token, bridge) }()
	t.Cleanup(func() { stopContractServer(t, listener, done) })
	header := http.Header{"Authorization": []string{"Bearer " + token}}
	ws, _, err := websocket.Dial(t.Context(), "ws://"+listener.Addr().String()+"/bridge", &websocket.DialOptions{HTTPHeader: header})
	if err != nil {
		t.Fatal(err)
	}
	conn := websocket.NetConn(t.Context(), ws, websocket.MessageText)
	defer func() { _ = conn.Close() }()
	got := make([]byte, len("bridge-ok"))
	if _, err := io.ReadFull(conn, got); err != nil {
		t.Fatal(err)
	}
	if string(got) != "bridge-ok" {
		t.Fatalf("bridge output = %q", got)
	}
}

func TestExecRPC_terminateReportsRunning(t *testing.T) {
	local := newLocalContractRunner(t).(*Local)
	spec := ProcessSpec{ID: "terminate", Argv: []string{"sh", "-c", "sleep 30"}, Dir: t.TempDir(), Sandbox: Sandbox{Mode: SandboxOff}}
	if err := local.Start(t.Context(), spec); err != nil {
		t.Fatal(err)
	}
	waitContractAlive(t, local, spec.ID)
	params, err := encodeSignalParams(spec.ID, "kill")
	if err != nil {
		t.Fatal(err)
	}
	result, err := callMethod(t.Context(), local, "process/terminate", params, trustedExecPolicy())
	if err != nil {
		t.Fatal(err)
	}
	fields, err := objectFields(result)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := fieldBool(fields, "running"); err != nil {
		t.Fatal(err)
	}
	state := waitForExit(t, local, spec.ID, 5*time.Second)
	if state.Exit == nil || state.Alive {
		t.Fatalf("terminated state = %+v", state)
	}
}

func requireExecRootLink(t *testing.T, root, data string) {
	t.Helper()
	resolved, err := filepath.EvalSymlinks(root)
	if err != nil {
		t.Fatal(err)
	}
	canonicalData, err := filepath.EvalSymlinks(data)
	if err != nil {
		t.Fatal(err)
	}
	if resolved != filepath.Join(canonicalData, "workspace") {
		t.Fatalf("resolved root = %q", resolved)
	}
}

func TestPrepareExecRoot_linksPersistentWorkspace(t *testing.T) {
	data := t.TempDir()
	root := filepath.Join(t.TempDir(), "workspace-link")
	t.Setenv("HOME", filepath.Join(t.TempDir(), "old-home"))
	got, err := prepareExecRoot(root, data)
	if err != nil {
		t.Fatal(err)
	}
	if got != root {
		t.Fatalf("root = %q, want %q", got, root)
	}
	requireExecRootLink(t, root, data)
	if os.Getenv("HOME") != filepath.Join(data, "home") {
		t.Fatalf("HOME = %q", os.Getenv("HOME"))
	}
	for _, path := range []string{filepath.Join(data, "workspace"), filepath.Join(data, "home")} {
		info, err := os.Stat(path)
		if err != nil {
			t.Fatal(err)
		}
		if !info.IsDir() {
			t.Fatalf("%s is not a directory", path)
		}
	}
}

func TestRemoteVersionMismatch(t *testing.T) {
	err := versionMismatch("box-1", "v1.2.3", "v1.2.4")
	want := "runner: box box-1 runs dalgona v1.2.3; this is v1.2.4; use the matching box image"
	if err == nil || err.Error() != want {
		t.Fatalf("error = %v, want %q", err, want)
	}
}

func TestParseRPC_rejectsTrailingValue(t *testing.T) {
	line := []byte(`{"jsonrpc":"2.0","id":1,"method":"initialize"} {}`)
	if _, err := parseRPC(line); err == nil {
		t.Fatal("parseRPC accepted a trailing JSON value")
	}
}

func TestDispatchRPC_suppressesNotificationErrors(t *testing.T) {
	var out bytes.Buffer
	writer := rpcWriter{w: &out, mu: &sync.Mutex{}}
	line := []byte(`{"jsonrpc":"2.0","method":"missing/method"}`)
	if err := dispatchRPC(t.Context(), writer, nil, line, trustedExecPolicy()); err != nil {
		t.Fatal(err)
	}
	if out.Len() != 0 {
		t.Fatalf("notification response = %s", out.Bytes())
	}
}

func TestBootstrap_uploadsThreeMiB(t *testing.T) {
	for name, wrap := range map[string]func(*shellStream) Stream{
		"half-close":    func(s *shellStream) Stream { return s },
		"term-on-close": func(s *shellStream) Stream { return termOnCloseStream{s} },
	} {
		t.Run(name, func(t *testing.T) { testBootstrapUpload(t, wrap) })
	}
}

func testBootstrapUpload(t *testing.T, wrap func(*shellStream) Stream) {
	arch, machine := foreignArchitecture(t)
	payload := writeSiblingBinary(t, arch)
	home := t.TempDir()
	var encoded bytes.Buffer
	run := func(ctx context.Context, argv []string) (Stream, error) {
		if len(argv) == 3 && argv[2] == probeScript {
			return &staticStream{reader: strings.NewReader(machine + "\nold\n")}, nil
		}
		shell, startErr := startShellStream(ctx, argv, home, &encoded)
		if startErr != nil {
			return nil, startErr
		}
		return wrap(shell), nil
	}
	ctx, cancel := context.WithTimeout(t.Context(), time.Minute)
	defer cancel()
	if bootErr := Bootstrap(ctx, "upload-box", run); bootErr != nil {
		t.Fatal(bootErr)
	}
	assertUploaded(t, filepath.Join(home, ".dalgona", "bin", "dalgona"), payload)
	assertBase64Lines(t, encoded.Bytes())
	if got, want := int64(encoded.Len()), base64Length(int64(len(payload))); got != want {
		t.Fatalf("encoded %d bytes, announced %d", got, want)
	}
}

func writeSiblingBinary(t *testing.T, arch string) []byte {
	t.Helper()
	executable, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	source := filepath.Join(filepath.Dir(executable), "dalgona-linux-"+arch)
	payload := make([]byte, 3<<20)
	if _, err = rand.Read(payload); err != nil {
		t.Fatal(err)
	}
	if err = os.WriteFile(source, payload, 0o755); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.Remove(source) })
	return payload
}

func assertUploaded(t *testing.T, path string, payload []byte) {
	t.Helper()
	uploaded, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if sha256.Sum256(uploaded) != sha256.Sum256(payload) {
		t.Fatal("uploaded binary SHA-256 differs from source")
	}
}

type termOnCloseStream struct{ shell *shellStream }

func (s termOnCloseStream) Read(p []byte) (int, error)  { return s.shell.Read(p) }
func (s termOnCloseStream) Write(p []byte) (int, error) { return s.shell.Write(p) }
func (s termOnCloseStream) Wait() (int, error)          { return s.shell.Wait() }

func (s termOnCloseStream) Close() error {
	if s.shell.cmd.Process != nil {
		_ = s.shell.cmd.Process.Signal(syscall.SIGTERM)
	}
	return s.shell.Close()
}

func foreignArchitecture(t *testing.T) (string, string) {
	t.Helper()
	if runtime.GOARCH == "amd64" {
		return "arm64", "aarch64"
	}
	if runtime.GOARCH == "arm64" {
		return "amd64", "x86_64"
	}
	t.Skip("test needs an amd64 or arm64 host")
	return "", ""
}

func assertBase64Lines(t *testing.T, encoded []byte) {
	t.Helper()
	lines := strings.Split(strings.TrimSuffix(string(encoded), "\n"), "\n")
	if len(lines) < 2 {
		t.Fatalf("base64 line count = %d", len(lines))
	}
	for i, line := range lines {
		if len(line) != 76 && i != len(lines)-1 {
			t.Fatalf("base64 line %d has %d columns", i+1, len(line))
		}
		if len(line) > 76 || len(line) == 0 {
			t.Fatalf("base64 line %d has %d columns", i+1, len(line))
		}
	}
}

func TestBootstrap_rejectsUnknownMachine(t *testing.T) {
	run := func(context.Context, []string) (Stream, error) {
		return &staticStream{reader: strings.NewReader("mips\n")}, nil
	}
	err := Bootstrap(t.Context(), "weird-box", run)
	want := "runner: box weird-box: unknown machine mips"
	if err == nil || err.Error() != want {
		t.Fatalf("error = %v, want %q", err, want)
	}
}

func TestBootstrap_skipsMatchingVersion(t *testing.T) {
	calls := 0
	run := func(_ context.Context, argv []string) (Stream, error) {
		calls++
		if len(argv) != 3 || argv[2] != probeScript {
			t.Fatalf("unexpected command after a matching probe: %q", argv)
		}
		return &staticStream{reader: strings.NewReader("x86_64\ndalgona " + localVersion() + "\n")}, nil
	}
	if err := Bootstrap(t.Context(), "current-box", run); err != nil {
		t.Fatal(err)
	}
	if calls != 1 {
		t.Fatalf("Bootstrap ran %d commands, want 1", calls)
	}
}

func TestProbeScriptKeepsVersionDiagnosticOnStdout(t *testing.T) {
	home := t.TempDir()
	var env []string
	for _, kv := range os.Environ() {
		if !strings.HasPrefix(kv, "HOME=") {
			env = append(env, kv)
		}
	}
	env = append(env, "HOME="+home)
	cmd := exec.Command("sh", "-c", probeScript)
	cmd.Env = env
	var stdout, stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr
	if err := cmd.Run(); err == nil {
		t.Fatal("probe succeeded with no dalgona binary under HOME; want the version check to fail")
	}
	archOut, err := exec.Command("uname", "-m").Output()
	if err != nil {
		t.Fatalf("uname -m: %v", err)
	}
	arch := strings.TrimSpace(string(archOut))
	line, _, _ := strings.Cut(stdout.String(), "\n")
	if strings.TrimSpace(line) != arch {
		t.Fatalf("probe first stdout line = %q, want arch %q (stdout %q, stderr %q)", line, arch, stdout.String(), stderr.String())
	}
	if stderr.Len() != 0 {
		t.Fatalf("probe wrote %q to stderr; the daemon forwards stderr in separate frames that can arrive before the arch line", stderr.String())
	}
}

func TestExecRPC_writeFileSyncsFilesystem(t *testing.T) {
	local := newLocalContractRunner(t).(*Local)
	saved := syncFS
	t.Cleanup(func() { syncFS = saved })
	path := filepath.Join(t.TempDir(), "synced")
	syncs := 0
	syncFS = func() error {
		syncs++
		data, err := os.ReadFile(path)
		if err != nil {
			return err
		}
		if string(data) != "durable" {
			return fmt.Errorf("sync before rename saw %q", data)
		}
		return nil
	}
	params, err := encodeWriteParams(path, []byte("durable"), 0o600, 0)
	if err != nil {
		t.Fatal(err)
	}
	if _, callErr := callMethod(t.Context(), local, "fs/writeFile", params, trustedExecPolicy()); callErr != nil {
		t.Fatal(callErr)
	}
	if syncs != 1 {
		t.Fatalf("syncFS calls = %d, want 1", syncs)
	}
}

type staticStream struct {
	reader *strings.Reader
}

func (s *staticStream) Read(p []byte) (int, error)  { return s.reader.Read(p) }
func (s *staticStream) Write(p []byte) (int, error) { return len(p), nil }
func (s *staticStream) Close() error                { return nil }
func (s *staticStream) Wait() (int, error)          { return 0, nil }

type shellStream struct {
	cmd     *exec.Cmd
	stdin   io.WriteCloser
	stdout  io.ReadCloser
	encoded *bytes.Buffer
}

func startShellStream(ctx context.Context, argv []string, home string, encoded *bytes.Buffer) (*shellStream, error) {
	cmd := exec.CommandContext(ctx, argv[0], argv[1:]...)
	cmd.Env = shellEnvironment(home)
	cmd.Stderr = os.Stderr
	stdin, err := cmd.StdinPipe()
	if err != nil {
		return nil, fmt.Errorf("stdin pipe: %w", err)
	}
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return nil, fmt.Errorf("stdout pipe: %w", err)
	}
	if err := cmd.Start(); err != nil {
		return nil, fmt.Errorf("start shell: %w", err)
	}
	return &shellStream{cmd: cmd, stdin: stdin, stdout: stdout, encoded: encoded}, nil
}

func shellEnvironment(home string) []string {
	env := os.Environ()
	filtered := env[:0]
	for _, value := range env {
		if !strings.HasPrefix(value, "HOME=") {
			filtered = append(filtered, value)
		}
	}
	return append(filtered, "HOME="+home)
}

func (s *shellStream) Read(p []byte) (int, error) { return s.stdout.Read(p) }

func (s *shellStream) Write(p []byte) (int, error) {
	s.encoded.Write(p)
	return s.stdin.Write(p)
}

func (s *shellStream) CloseWrite() error { return s.stdin.Close() }

func (s *shellStream) Close() error {
	_ = s.stdin.Close()
	_ = s.stdout.Close()
	return nil
}

func (s *shellStream) Wait() (int, error) {
	err := s.cmd.Wait()
	if err == nil {
		return 0, nil
	}
	exitErr, ok := errors.AsType[*exec.ExitError](err)
	if ok {
		return exitErr.ExitCode(), nil
	}
	return 0, fmt.Errorf("wait shell: %w", err)
}

func TestUpload_matchesIndependentGitTree(t *testing.T) {
	source := t.TempDir()
	writeTestFile(t, filepath.Join(source, "main.go"), []byte("package main\n"), 0o644)
	writeTestFile(t, filepath.Join(source, "tool.sh"), []byte("#!/bin/sh\nexit 0\n"), 0o755)
	writeTestFile(t, filepath.Join(source, ".gitignore"), []byte("ignored.txt\n"), 0o644)
	writeTestFile(t, filepath.Join(source, "ignored.txt"), []byte("not uploaded\n"), 0o644)
	runGit(t, source, "init", "-q")
	runGit(t, source, "add", "-A")
	want := runGit(t, source, "write-tree")
	writeTestFile(t, filepath.Join(source, ".dalgona-baseline"), []byte("stale\n"), 0o644)
	workspace := t.TempDir()
	local, err := NewLocal(filepath.Join(t.TempDir(), "runs"))
	if err != nil {
		t.Fatal(err)
	}
	r := &workspaceRunner{local: local, workspace: workspace}
	baseline, err := Upload(t.Context(), r, source)
	if err != nil {
		t.Fatal(err)
	}
	if baseline != want {
		t.Fatalf("baseline = %q, want %q", baseline, want)
	}
	assertUploadedWorkspace(t, source, workspace, want)
}

func writeTestFile(t *testing.T, path string, data []byte, mode fs.FileMode) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, data, mode); err != nil {
		t.Fatal(err)
	}
}

func runGit(t *testing.T, dir string, args ...string) string {
	t.Helper()
	argv := make([]string, 0, 2+len(args))
	argv = append(argv, "-C", dir)
	argv = append(argv, args...)
	cmd := exec.CommandContext(t.Context(), "git", argv...)
	out, err := cmd.CombinedOutput()
	if err != nil {
		t.Fatalf("git %v: %v: %s", args, err, out)
	}
	return strings.TrimSpace(string(out))
}

func assertUploadedWorkspace(t *testing.T, source, workspace, baseline string) {
	t.Helper()
	got, err := os.ReadFile(filepath.Join(workspace, "main.go"))
	if err != nil {
		t.Fatal(err)
	}
	want, err := os.ReadFile(filepath.Join(source, "main.go"))
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(got, want) {
		t.Fatalf("uploaded main.go = %q, want %q", got, want)
	}
	info, err := os.Stat(filepath.Join(workspace, "tool.sh"))
	if err != nil {
		t.Fatal(err)
	}
	if info.Mode().Perm()&0o111 == 0 {
		t.Fatalf("tool.sh mode = %v", info.Mode())
	}
	stored, err := os.ReadFile(filepath.Join(workspace, ".dalgona-baseline"))
	if err != nil {
		t.Fatal(err)
	}
	if string(stored) != baseline {
		t.Fatalf("baseline file = %q, want %q", stored, baseline)
	}
	if _, err := os.Stat(filepath.Join(workspace, "ignored.txt")); !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("ignored file error = %v", err)
	}
}

func TestUploadEnumeration_skipsExistingBaseline(t *testing.T) {
	cwd := t.TempDir()
	writeTestFile(t, filepath.Join(cwd, ".dalgona-baseline"), []byte("stale"), 0o644)
	calls := 0
	emit := func(uploadJob) error {
		calls++
		return nil
	}
	if err := emitRel(cwd, ".dalgona-baseline", emit); err != nil {
		t.Fatal(err)
	}
	if err := enumerate(t.Context(), cwd, emit); err != nil {
		t.Fatal(err)
	}
	if calls != 0 {
		t.Fatalf("baseline emitted %d times", calls)
	}
}

type workspaceRunner struct {
	local     *Local
	workspace string
}

func (r *workspaceRunner) path(path string) string {
	if path == workspaceRoot {
		return r.workspace
	}
	rel, ok := strings.CutPrefix(path, workspaceRoot+"/")
	if ok {
		return filepath.Join(r.workspace, filepath.FromSlash(rel))
	}
	return path
}

func (r *workspaceRunner) Start(ctx context.Context, spec ProcessSpec) error {
	spec.Dir = r.path(spec.Dir)
	argv := make([]string, len(spec.Argv))
	for i, value := range spec.Argv {
		argv[i] = strings.ReplaceAll(value, workspaceRoot, r.workspace)
	}
	spec.Argv = argv
	return r.local.Start(ctx, spec)
}

func (r *workspaceRunner) Read(ctx context.Context, id string, offset int64) (Output, error) {
	return r.local.Read(ctx, id, offset)
}

func (r *workspaceRunner) Signal(ctx context.Context, id string, signal Signal) error {
	return r.local.Signal(ctx, id, signal)
}

func (r *workspaceRunner) ReadFile(ctx context.Context, path string) ([]byte, error) {
	return r.local.ReadFile(ctx, r.path(path))
}

func (r *workspaceRunner) WriteFile(ctx context.Context, path string, data []byte, mode fs.FileMode) error {
	return r.local.WriteFile(ctx, r.path(path), data, mode)
}

func (r *workspaceRunner) Stat(ctx context.Context, path string) (FileInfo, error) {
	return r.local.Stat(ctx, r.path(path))
}

func (r *workspaceRunner) ReadDir(ctx context.Context, path string) ([]FileInfo, error) {
	return r.local.ReadDir(ctx, r.path(path))
}

func (r *workspaceRunner) Remove(ctx context.Context, path string) error {
	return r.local.Remove(ctx, r.path(path))
}

func (r *workspaceRunner) Close() error { return r.local.Close() }
