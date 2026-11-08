package runner

import (
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"slices"
	"strconv"
	"strings"
	"time"
)

const shimExecEnvironment = "DALGONA_SHIM_EXEC"

func ShimMain(args []string) int {
	dir, argv, err := parseShimArgs(args)
	if err != nil {
		return 2
	}
	if os.Getenv(shimExecEnvironment) == "1" {
		if err := execSandboxed(dir, argv); err != nil {
			_, _ = fmt.Fprintf(os.Stderr, "dalgona: %v\n", err)
			return 1
		}
		return 0
	}
	if err := runShim(dir, argv); err != nil {
		return 1
	}
	return 0
}

func parseShimArgs(args []string) (string, []string, error) {
	if len(args) < 4 || args[0] != "--dir" || args[2] != "--" {
		return "", nil, errors.New("runner: usage __shim --dir <directory> -- <command>")
	}
	return args[1], args[3:], nil
}

func runShim(dir string, argv []string) error {
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return fmt.Errorf("runner: create shim directory: %w", err)
	}
	out, err := os.OpenFile(filepath.Join(dir, "out"), os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o600)
	if err != nil {
		return fmt.Errorf("runner: open shim output: %w", err)
	}
	if pidErr := writePID(dir); pidErr != nil {
		return failShim(dir, out, pidErr)
	}
	cmd, err := commandForShim(argv)
	if err != nil {
		return failShim(dir, out, err)
	}
	if sandboxErr := configureSandbox(cmd, dir, currentSandbox()); sandboxErr != nil {
		return failShim(dir, out, fmt.Errorf("sandbox: %w", sandboxErr))
	}
	writer := &limitedWriter{w: out, limit: outputLimit}
	cmd.Stdout = writer
	cmd.Stderr = writer
	configureProcessGroup(cmd)
	if startErr := cmd.Start(); startErr != nil {
		return failShim(dir, out, startErr)
	}
	if groupErr := writeProcessGroup(dir, cmd.Process.Pid); groupErr != nil {
		_ = cmd.Process.Kill()
		_ = cmd.Wait()
		return failShim(dir, out, groupErr)
	}
	cleanup, err := assignJob(cmd)
	if err != nil {
		_ = cmd.Process.Kill()
		_ = cmd.Wait()
		return failShim(dir, out, err)
	}
	waitErr := cmd.Wait()
	cleanup()
	code, signal := exitDetails(waitErr)
	closeErr := out.Close()
	finishErr := finishShim(dir, Exit{Code: code, Signal: signal, EndedAt: time.Now()})
	return errors.Join(closeErr, finishErr)
}

func commandForShim(argv []string) (*exec.Cmd, error) {
	path, err := exec.LookPath(argv[0])
	if err != nil {
		return nil, fmt.Errorf("runner: resolve command: %w", err)
	}
	cmd := &exec.Cmd{Path: path, Args: slices.Clone(argv)}
	cmd.Dir = os.Getenv("DALGONA_RUN_CWD")
	cmd.Env = shimEnv(os.Environ())
	cmd.Args[0] = path
	return cmd, nil
}

func currentSandbox() Sandbox {
	return Sandbox{
		Mode:          SandboxMode(os.Getenv("DALGONA_SANDBOX_MODE")),
		WritableRoots: splitRoots(os.Getenv("DALGONA_SANDBOX_ROOTS")),
	}
}

func writePID(dir string) error {
	pid := os.Getpid()
	start := processStartTime(pid)
	if start == "" {
		return errors.New("runner: process start time is unavailable")
	}
	data := []byte(strconv.Itoa(pid) + " " + start + "\n")
	if err := writeAtomic(filepath.Join(dir, "pid"), data, 0o600); err != nil {
		return fmt.Errorf("runner: write pid: %w", err)
	}
	if err := syncDir(dir); err != nil {
		return fmt.Errorf("runner: sync pid directory: %w", err)
	}
	return nil
}

func writeProcessGroup(dir string, pgid int) error {
	data := []byte(strconv.Itoa(pgid) + "\n")
	if err := writeAtomic(filepath.Join(dir, "pgid"), data, 0o600); err != nil {
		return fmt.Errorf("runner: write process group: %w", err)
	}
	if err := syncDir(dir); err != nil {
		return fmt.Errorf("runner: sync process group directory: %w", err)
	}
	return nil
}

func failShim(dir string, out *os.File, cause error) error {
	_, writeErr := io.WriteString(out, "dalgona: "+cause.Error()+"\n")
	closeErr := out.Close()
	finishErr := finishShim(dir, Exit{Code: 1, EndedAt: time.Now()})
	return errors.Join(cause, writeErr, closeErr, finishErr)
}

func finishShim(dir string, exit Exit) error {
	if err := writeExit(filepath.Join(dir, "exit"), exit); err != nil {
		return err
	}
	if err := syncFS(); err != nil {
		return fmt.Errorf("runner: sync filesystem: %w", err)
	}
	return nil
}

type limitedWriter struct {
	w       io.Writer
	written int64
	limit   int64
	dropped bool
}

func (w *limitedWriter) Write(p []byte) (int, error) {
	remaining := w.limit - w.written
	overflow := int64(len(p)) > remaining
	keep := p
	if overflow {
		keep = keep[:remaining]
	}
	if len(keep) > 0 {
		n, err := w.w.Write(keep)
		w.written += int64(n)
		if err != nil {
			return n, fmt.Errorf("runner: write output: %w", err)
		}
	}
	if overflow && !w.dropped {
		w.dropped = true
		if _, err := io.WriteString(w.w, "\n[dalgona: output beyond 1 GiB discarded]\n"); err != nil {
			return 0, fmt.Errorf("runner: write output notice: %w", err)
		}
	}
	return len(p), nil
}

var environmentDefaults = [...]string{
	"CI=1",
	"DEBIAN_FRONTEND=noninteractive",
	"GIT_TERMINAL_PROMPT=0",
	"GIT_EDITOR=:",
	"GIT_SEQUENCE_EDITOR=:",
	"GIT_MERGE_AUTOEDIT=no",
	"GIT_PAGER=cat",
	"PAGER=cat",
	"EDITOR=:",
	"npm_config_yes=true",
	"PIP_NO_INPUT=1",
}

func shimEnv(env []string) []string {
	result := slices.Clone(env)
	seen := make(map[string]bool, len(env))
	for _, value := range env {
		key, _, ok := strings.Cut(value, "=")
		if ok {
			seen[canonicalEnvironmentKey(key)] = true
		}
	}
	for _, value := range environmentDefaults {
		key, _, _ := strings.Cut(value, "=")
		if !seen[canonicalEnvironmentKey(key)] {
			result = append(result, value)
		}
	}
	return result
}

func canonicalEnvironmentKey(key string) string {
	if runtime.GOOS == "windows" {
		return strings.ToUpper(key)
	}
	return key
}

func setEnvironment(env []string, key string, value string) []string {
	result := removeEnvironment(env, key)
	return append(result, key+"="+value)
}

func removeEnvironment(env []string, key string) []string {
	result := make([]string, 0, len(env)+1)
	for _, item := range env {
		name, _, ok := strings.Cut(item, "=")
		if ok && name == key {
			continue
		}
		result = append(result, item)
	}
	return result
}

func splitRoots(value string) []string {
	if value == "" {
		return nil
	}
	return strings.Split(value, string(os.PathListSeparator))
}

var syncFS = syncConfiguredFilesystem

func syncConfiguredFilesystem() error {
	path := os.Getenv("DALGONA_SYNCFS")
	if path == "" {
		return nil
	}
	return syncFilesystem(path)
}

func parseExit(data []byte) (*Exit, error) {
	entries, err := exitEntries(data)
	if err != nil {
		return nil, err
	}
	var exit Exit
	for name, value := range entries {
		if err := setExitField(&exit, name, value); err != nil {
			return nil, err
		}
	}
	if _, ok := entries["code"]; !ok {
		return nil, errors.New("runner: exit record has no code")
	}
	if _, ok := entries["ended_at"]; !ok {
		return nil, errors.New("runner: exit record has no ended_at")
	}
	return &exit, nil
}

func setExitField(exit *Exit, name string, value string) error {
	switch name {
	case "code":
		code, err := strconv.Atoi(value)
		if err != nil {
			return fmt.Errorf("runner: invalid exit code: %w", err)
		}
		exit.Code = code
	case "signal":
		exit.Signal = value
	case "ended_at":
		when, err := time.Parse(time.RFC3339Nano, value)
		if err != nil {
			return fmt.Errorf("runner: invalid exit record: %w", err)
		}
		exit.EndedAt = when
	}
	return nil
}

func exitEntries(data []byte) (map[string]string, error) {
	text := strings.TrimSpace(string(data))
	if len(text) < 2 || text[0] != '{' || text[len(text)-1] != '}' {
		return nil, errors.New("runner: invalid exit record")
	}
	result := make(map[string]string, 3)
	for _, field := range strings.Split(text[1:len(text)-1], ",") {
		name, value, ok := strings.Cut(field, ":")
		if !ok {
			return nil, errors.New("runner: invalid exit record field")
		}
		name = strings.Trim(strings.TrimSpace(name), `"`)
		value = strings.Trim(strings.TrimSpace(value), `"`)
		result[name] = value
	}
	return result, nil
}
