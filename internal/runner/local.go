package runner

import (
	"cmp"
	"context"
	"encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"log/slog"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"time"
)

var outputLimit int64 = 1 << 30

var ErrProcessNotFound = errors.New("runner: process not found")

type Local struct {
	dir string
}

type processIdentity struct {
	pid   int
	start string
}

func NewLocal(dir string) (*Local, error) {
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return nil, fmt.Errorf("runner: create runs directory: %w", err)
	}
	return &Local{dir: dir}, nil
}

func (l *Local) Start(ctx context.Context, spec ProcessSpec) error {
	if err := ctx.Err(); err != nil {
		return fmt.Errorf("runner: start: %w", err)
	}
	if err := validateProcessSpec(spec); err != nil {
		return err
	}
	runDir := filepath.Join(l.dir, spec.ID)
	if err := os.Mkdir(runDir, 0o700); err != nil {
		if errors.Is(err, fs.ErrExist) {
			return nil
		}
		return fmt.Errorf("runner: create process directory: %w", err)
	}
	if err := l.spawn(runDir, spec); err != nil {
		_ = os.Remove(runDir)
		return err
	}
	return nil
}

func validateProcessSpec(spec ProcessSpec) error {
	if spec.ID == "" || len(spec.Argv) == 0 {
		return errors.New("runner: process id and argv are required")
	}
	if spec.ID == "." || filepath.Base(spec.ID) != spec.ID {
		return errors.New("runner: process id must be one path component")
	}
	return nil
}

func (l *Local) spawn(runDir string, spec ProcessSpec) error {
	self, err := os.Executable()
	if err != nil {
		return fmt.Errorf("runner: executable: %w", err)
	}
	argv := slices.Clone(spec.Argv)
	if argv[0] == "@self" {
		argv[0] = self
	}
	args := []string{"__shim", "--dir", runDir, "--"}
	args = append(args, argv...)
	mode := cmp.Or(spec.Sandbox.Mode, SandboxWorkspaceWrite)
	roots := slices.Clone(spec.Sandbox.WritableRoots)
	if spec.Dir != "" {
		roots = append(roots, spec.Dir)
	}
	spec.Env = runnerEnvironment(spec.Env, spec.Dir, mode, roots)
	cmd, err := startDetached(context.Background(), self, args, spec)
	if err != nil {
		return err
	}
	go func() {
		if err := cmd.Wait(); err != nil {
			slog.Debug("runner: shim exited without a clean status", "error", err)
		}
	}()
	return nil
}

func runnerEnvironment(base []string, dir string, mode SandboxMode, roots []string) []string {
	if len(base) == 0 {
		base = os.Environ()
	}
	env := withoutRunnerEnvironment(base)
	if mode != SandboxOff {
		env = withoutHomeEnvironment(env)
	}
	env = append(env,
		"DALGONA_RUN_CWD="+dir,
		"DALGONA_SANDBOX_MODE="+string(mode),
		"DALGONA_SANDBOX_ROOTS="+encodeRoots(roots),
		"DALGONA_SHIM_EXEC=0",
	)
	return env
}

func withoutHomeEnvironment(env []string) []string {
	result := make([]string, 0, len(env))
	for _, value := range env {
		key, _, ok := strings.Cut(value, "=")
		if ok && (key == "DALGONA_HOME" || key == "DAL_HOME") {
			continue
		}
		result = append(result, value)
	}
	return result
}

func encodeRoots(roots []string) string {
	data, err := json.Marshal(roots)
	if err != nil {
		return "[]"
	}
	return string(data)
}

func withoutRunnerEnvironment(env []string) []string {
	result := make([]string, 0, len(env)+4)
	for _, value := range env {
		key, _, ok := strings.Cut(value, "=")
		if ok && isRunnerEnvironment(key) {
			continue
		}
		result = append(result, value)
	}
	return result
}

func isRunnerEnvironment(key string) bool {
	if key == boxEnvironmentKey {
		return true
	}
	switch key {
	case "DALGONA_RUN_CWD", "DALGONA_SANDBOX_MODE", "DALGONA_SANDBOX_ROOTS", "DALGONA_SHIM_EXEC":
		return true
	default:
		return false
	}
}

func (l *Local) Read(ctx context.Context, id string, offset int64) (Output, error) {
	if err := ctx.Err(); err != nil {
		return Output{}, fmt.Errorf("runner: read: %w", err)
	}
	if offset < 0 {
		return Output{}, errors.New("runner: negative output offset")
	}
	runDir := filepath.Join(l.dir, id)
	if _, err := os.Stat(runDir); err != nil {
		if errors.Is(err, fs.ErrNotExist) {
			return Output{}, ErrProcessNotFound
		}
		return Output{}, fmt.Errorf("runner: stat process: %w", err)
	}
	data, err := readAt(runDir, offset)
	if err != nil {
		return Output{}, err
	}
	exit, err := readExit(filepath.Join(runDir, "exit"))
	if err != nil && !errors.Is(err, fs.ErrNotExist) {
		return Output{}, err
	}
	alive := exit == nil && processAlive(filepath.Join(runDir, "pid"))
	return Output{Data: data, Next: offset + int64(len(data)), Exit: exit, Alive: alive, Path: filepath.Join(runDir, "out")}, nil
}

func (l *Local) Signal(ctx context.Context, id string, signal Signal) error {
	if err := ctx.Err(); err != nil {
		return fmt.Errorf("runner: signal: %w", err)
	}
	runDir := filepath.Join(l.dir, id)
	if exited, err := exitRecorded(runDir); exited || err != nil {
		return err
	}
	if err := requestCancel(runDir, signal); err != nil {
		return err
	}
	return signalStarted(runDir, signal)
}

func exitRecorded(runDir string) (bool, error) {
	_, err := os.Stat(filepath.Join(runDir, "exit"))
	if err == nil {
		return true, nil
	}
	if errors.Is(err, fs.ErrNotExist) {
		return false, nil
	}
	return false, fmt.Errorf("runner: stat exit: %w", err)
}

func requestCancel(runDir string, signal Signal) error {
	data := []byte("TERM\n")
	if signal == SignalKill {
		data = []byte("KILL\n")
	}
	if err := writeAtomic(filepath.Join(runDir, "cancel"), data, 0o600); err != nil {
		if errors.Is(err, fs.ErrNotExist) {
			return ErrProcessNotFound
		}
		return fmt.Errorf("runner: write cancel request: %w", err)
	}
	if err := syncDir(runDir); err != nil {
		return fmt.Errorf("runner: sync cancel request: %w", err)
	}
	return nil
}

func signalStarted(runDir string, signal Signal) error {
	identity, err := readIdentity(filepath.Join(runDir, "pid"))
	if errors.Is(err, fs.ErrNotExist) {
		return nil
	}
	if err != nil {
		return err
	}
	if !identityAlive(identity) {
		return missingUnlessExited(runDir)
	}
	target, err := readProcessGroup(runDir)
	if errors.Is(err, fs.ErrNotExist) {
		return nil
	}
	if err != nil {
		return err
	}
	return signalProcess(identity.pid, target, signal)
}

func missingUnlessExited(runDir string) error {
	exited, err := exitRecorded(runDir)
	if err != nil || exited {
		return err
	}
	return ErrProcessNotFound
}

func readProcessGroup(runDir string) (int, error) {
	data, err := os.ReadFile(filepath.Join(runDir, "pgid"))
	if err != nil {
		return 0, fmt.Errorf("runner: read process group: %w", err)
	}
	target, err := strconv.Atoi(strings.TrimSpace(string(data)))
	if err != nil || target <= 0 {
		return 0, errors.New("runner: invalid process group")
	}
	return target, nil
}

func readCancel(runDir string) (Signal, bool) {
	data, err := os.ReadFile(filepath.Join(runDir, "cancel"))
	if errors.Is(err, fs.ErrNotExist) {
		return 0, false
	}
	if err == nil && strings.TrimSpace(string(data)) == "KILL" {
		return SignalKill, true
	}
	return SignalTerm, true
}

func (l *Local) ReadFile(ctx context.Context, path string) ([]byte, error) {
	if err := ctx.Err(); err != nil {
		return nil, fmt.Errorf("runner: read file: %w", err)
	}
	file, err := openNoFollow(path)
	if err != nil {
		return nil, fmt.Errorf("runner: read file %s: %w", path, err)
	}
	defer func() { _ = file.Close() }()
	data, err := io.ReadAll(file)
	if err != nil {
		return nil, fmt.Errorf("runner: read file %s: %w", path, err)
	}
	return data, nil
}

func (l *Local) WriteFile(ctx context.Context, path string, data []byte, perm fs.FileMode) error {
	if err := ctx.Err(); err != nil {
		return fmt.Errorf("runner: write file: %w", err)
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return fmt.Errorf("runner: create parent: %w", err)
	}
	if err := writeAtomic(path, data, perm); err != nil {
		return fmt.Errorf("runner: write file %s: %w", path, err)
	}
	if err := syncDir(filepath.Dir(path)); err != nil {
		return fmt.Errorf("runner: sync parent: %w", err)
	}
	return nil
}

func (l *Local) Stat(ctx context.Context, path string) (FileInfo, error) {
	if err := ctx.Err(); err != nil {
		return FileInfo{}, fmt.Errorf("runner: stat: %w", err)
	}
	st, err := statNoFollow(path)
	if err != nil {
		return FileInfo{}, fmt.Errorf("runner: stat %s: %w", path, err)
	}
	return fileInfo(st), nil
}

func (l *Local) ReadDir(ctx context.Context, path string) ([]FileInfo, error) {
	if err := ctx.Err(); err != nil {
		return nil, fmt.Errorf("runner: read directory: %w", err)
	}
	entries, err := os.ReadDir(path)
	if err != nil {
		return nil, fmt.Errorf("runner: read directory %s: %w", path, err)
	}
	result := make([]FileInfo, 0, len(entries))
	for _, entry := range entries {
		info, err := entry.Info()
		if err != nil {
			return nil, fmt.Errorf("runner: read directory entry: %w", err)
		}
		result = append(result, fileInfo(info))
	}
	return result, nil
}

func fileInfo(info fs.FileInfo) FileInfo {
	return FileInfo{Name: info.Name(), Size: info.Size(), Mode: info.Mode(), ModTime: info.ModTime()}
}

func (l *Local) Remove(ctx context.Context, path string) error {
	if err := ctx.Err(); err != nil {
		return fmt.Errorf("runner: remove: %w", err)
	}
	if err := os.RemoveAll(path); err != nil {
		return fmt.Errorf("runner: remove %s: %w", path, err)
	}
	return nil
}

func (l *Local) Close() error { return nil }

func readAt(dir string, offset int64) ([]byte, error) {
	file, err := os.Open(filepath.Join(dir, "out"))
	if err != nil {
		if errors.Is(err, fs.ErrNotExist) {
			return nil, nil
		}
		return nil, fmt.Errorf("runner: open output: %w", err)
	}
	defer func() { _ = file.Close() }()
	if _, seekErr := file.Seek(offset, io.SeekStart); seekErr != nil {
		return nil, fmt.Errorf("runner: seek output: %w", seekErr)
	}
	data, err := io.ReadAll(file)
	if err != nil {
		return nil, fmt.Errorf("runner: read output: %w", err)
	}
	return data, nil
}

func readIdentity(path string) (processIdentity, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return processIdentity{}, fmt.Errorf("runner: read pid: %w", err)
	}
	parts := strings.Fields(string(data))
	if len(parts) != 2 {
		return processIdentity{}, errors.New("runner: invalid pid file")
	}
	pid, err := strconv.Atoi(parts[0])
	if err != nil || pid <= 0 || parts[1] == "" {
		return processIdentity{}, errors.New("runner: invalid pid file")
	}
	return processIdentity{pid: pid, start: parts[1]}, nil
}

func processAlive(path string) bool {
	identity, err := readIdentity(path)
	return err == nil && identityAlive(identity)
}

func identityAlive(identity processIdentity) bool {
	if !processIsAlive(identity.pid) {
		return false
	}
	return processStartTime(identity.pid) == identity.start
}

func readExit(path string) (*Exit, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("runner: read exit: %w", err)
	}
	exit, err := parseExit(data)
	if err != nil {
		return nil, fmt.Errorf("runner: read exit: %w", err)
	}
	return exit, nil
}

func writeExit(path string, exit Exit) error {
	data := []byte(`{"code":` + strconv.Itoa(exit.Code) + `,"signal":"` + escapeJSON(exit.Signal) + `","ended_at":"` + exit.EndedAt.UTC().Format(time.RFC3339Nano) + `"}`)
	if err := writeAtomic(path, data, 0o600); err != nil {
		return fmt.Errorf("runner: write exit: %w", err)
	}
	if err := syncDir(filepath.Dir(path)); err != nil {
		return fmt.Errorf("runner: sync exit directory: %w", err)
	}
	return nil
}

func writeAtomic(path string, data []byte, perm fs.FileMode) error {
	tmp, err := os.CreateTemp(filepath.Dir(path), ".dalgona-write-")
	if err != nil {
		return fmt.Errorf("create temporary file: %w", err)
	}
	name := tmp.Name()
	defer func() { _ = os.Remove(name) }()
	if err := tmp.Chmod(perm); err != nil {
		_ = tmp.Close()
		return fmt.Errorf("set temporary mode: %w", err)
	}
	if _, err := tmp.Write(data); err != nil {
		_ = tmp.Close()
		return fmt.Errorf("write temporary file: %w", err)
	}
	if err := tmp.Sync(); err != nil {
		_ = tmp.Close()
		return fmt.Errorf("sync temporary file: %w", err)
	}
	if err := tmp.Close(); err != nil {
		return fmt.Errorf("close temporary file: %w", err)
	}
	if err := os.Rename(name, path); err != nil {
		return fmt.Errorf("rename temporary file: %w", err)
	}
	return nil
}

func escapeJSON(value string) string {
	replacer := strings.NewReplacer(
		`\`, `\\`, `"`, `\"`, "\n", `\n`, "\r", `\r`, "\t", `\t`, "\b", `\b`, "\f", `\f`,
	)
	return replacer.Replace(value)
}
