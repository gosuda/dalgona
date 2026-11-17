package runner

import (
	"bytes"
	"cmp"
	"context"
	"errors"
	"log/slog"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"
)

const (
	shellEnvTimeout = 5 * time.Second
	shellEnvLimit   = 1 << 20
)

var shellEnvVolatile = [...]string{"_", "SHLVL", "PWD", "OLDPWD"}

type ShellEnvSnapshot struct {
	mu          sync.Mutex
	env         []string
	fingerprint string
	resolved    bool
	shell       string
	isSecret    func(string) bool
	timeout     time.Duration
	limit       int
	log         *slog.Logger
}

type shellEnvError struct {
	class  string
	status int
}

func (e shellEnvError) Error() string {
	return "runner: login shell environment: " + e.class
}

type cappedBuffer struct {
	buf   bytes.Buffer
	limit int
	over  bool
}

func (c *cappedBuffer) Write(p []byte) (int, error) {
	n := len(p)
	if room := c.limit - c.buf.Len(); n > room {
		c.over = true
		p = p[:room]
	}
	c.buf.Write(p)
	return n, nil
}

func NewShellEnvSnapshot(shell string, isSecret func(string) bool) *ShellEnvSnapshot {
	return &ShellEnvSnapshot{shell: shell, isSecret: isSecret, timeout: shellEnvTimeout, limit: shellEnvLimit}
}

func (s *ShellEnvSnapshot) Resolve(ctx context.Context, daemonEnv []string) []string {
	if s == nil || s.isSecret == nil {
		return slices.Clone(daemonEnv)
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	shell := s.shellPath()
	if shell == "" {
		return slices.Clone(daemonEnv)
	}
	fingerprint := shellEnvFingerprint(shell, daemonEnv)
	if !s.resolved || s.fingerprint != fingerprint {
		s.refresh(ctx, shell, fingerprint, daemonEnv)
	}
	return overlayEnvironment(daemonEnv, s.env)
}

func (s *ShellEnvSnapshot) shellPath() string {
	if s.shell != "" {
		return s.shell
	}
	if runtime.GOOS == "windows" {
		return ""
	}
	return "/bin/sh"
}

func (s *ShellEnvSnapshot) refresh(ctx context.Context, shell, fingerprint string, daemonEnv []string) {
	delta, err := s.captureDelta(ctx, shell, daemonEnv)
	if err != nil && ctx.Err() != nil {
		return
	}
	s.env, s.fingerprint, s.resolved = delta, fingerprint, true
	if err != nil {
		s.warn(err)
	}
}

func (s *ShellEnvSnapshot) captureDelta(ctx context.Context, shell string, daemonEnv []string) ([]string, error) {
	raw, err := runShellEnv(ctx, shell, daemonEnv, s.timeout, s.limit)
	if err != nil {
		return nil, err
	}
	defer clear(raw)
	records, ok := parseShellEnv(raw)
	if !ok {
		return nil, shellEnvError{class: "malformed"}
	}
	return s.changed(records, daemonEnv), nil
}

func (s *ShellEnvSnapshot) changed(records, daemonEnv []string) []string {
	known := make(map[string]string, len(daemonEnv))
	for _, item := range daemonEnv {
		name, value, _ := strings.Cut(item, "=")
		known[canonicalEnvironmentKey(name)] = value
	}
	delta := make(map[string]string)
	for _, record := range records {
		name, value, _ := strings.Cut(record, "=")
		if slices.Contains(shellEnvVolatile[:], name) || s.isSecret(name) {
			continue
		}
		if current, ok := known[canonicalEnvironmentKey(name)]; ok && current == value {
			continue
		}
		delta[name] = value
	}
	return envList(delta)
}

func (s *ShellEnvSnapshot) warn(err error) {
	failure, _ := errors.AsType[shellEnvError](err)
	attrs := []any{"class", failure.class}
	if failure.class == "exit" {
		attrs = append(attrs, "status", failure.status)
	}
	cmp.Or(s.log, slog.Default()).Warn("runner: login shell environment unavailable, using the daemon environment", attrs...)
}

func runShellEnv(ctx context.Context, shell string, env []string, timeout time.Duration, limit int) ([]byte, error) {
	ctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()
	out := &cappedBuffer{limit: limit}
	defer func() { clear(out.buf.Bytes()) }()
	cmd := exec.Command(shell, "-lic", "env")
	cmd.Env = slices.Clone(env)
	cmd.Stdout = out
	cmd.WaitDelay = outputDrainDelay
	configureShellCapture(cmd)
	if err := cmd.Start(); err != nil {
		return nil, shellEnvError{class: "start"}
	}
	var waitErr error
	done := make(chan struct{})
	go func() {
		waitErr = cmd.Wait()
		close(done)
	}()
	select {
	case <-done:
		return finishShellEnv(cmd, out, waitErr)
	case <-ctx.Done():
		stopShellEnv(cmd)
		<-done
		return nil, shellEnvError{class: contextClass(ctx)}
	}
}

func finishShellEnv(cmd *exec.Cmd, out *cappedBuffer, waitErr error) ([]byte, error) {
	stopShellEnv(cmd)
	if out.over {
		return nil, shellEnvError{class: "overflow"}
	}
	if exitErr, ok := errors.AsType[*exec.ExitError](waitErr); ok {
		return nil, shellEnvError{class: "exit", status: exitErr.ExitCode()}
	}
	if waitErr != nil {
		return nil, shellEnvError{class: "wait"}
	}
	return bytes.Clone(out.buf.Bytes()), nil
}

func stopShellEnv(cmd *exec.Cmd) {
	killProcessGroupOnExit(cmd.Process.Pid)
	_ = cmd.Process.Kill()
}

func contextClass(ctx context.Context) string {
	if errors.Is(ctx.Err(), context.DeadlineExceeded) {
		return "timeout"
	}
	return "canceled"
}

func parseShellEnv(raw []byte) ([]string, bool) {
	if bytes.IndexByte(raw, 0) >= 0 {
		return nil, false
	}
	var records []string
	for line := range strings.SplitSeq(string(raw), "\n") {
		name, _, ok := strings.Cut(line, "=")
		if !ok {
			continue
		}
		if !validEnvironmentName(name) {
			return nil, false
		}
		records = append(records, line)
	}
	return records, len(records) > 0
}

func validEnvironmentName(name string) bool {
	if name == "" || name[0] >= '0' && name[0] <= '9' {
		return false
	}
	return !strings.ContainsFunc(name, func(r rune) bool { return !environmentNameRune(r) })
}

func environmentNameRune(r rune) bool {
	return r == '_' || r >= '0' && r <= '9' || r >= 'a' && r <= 'z' || r >= 'A' && r <= 'Z'
}

func shellEnvFingerprint(shell string, env []string) string {
	home := environmentValue(env, "HOME")
	if home == "" {
		home, _ = os.UserHomeDir()
	}
	var out strings.Builder
	out.WriteString(shell + "\x00" + home)
	for _, name := range shellStartupFiles(shell) {
		out.WriteString("\x00" + name + ":" + startupFileState(home, name))
	}
	return out.String()
}

func shellStartupFiles(shell string) []string {
	switch filepath.Base(shell) {
	case "zsh":
		return []string{".zshenv", ".zprofile", ".zshrc", ".zlogin"}
	case "bash":
		return []string{".bash_profile", ".bash_login", ".profile", ".bashrc"}
	default:
		return []string{".profile"}
	}
}

func startupFileState(home, name string) string {
	if home == "" {
		return "-"
	}
	info, err := os.Stat(filepath.Join(home, name))
	if err != nil {
		return "-"
	}
	return strconv.FormatInt(info.Size(), 10) + ":" + strconv.FormatInt(info.ModTime().UnixNano(), 10)
}

func environmentValue(env []string, name string) string {
	want := canonicalEnvironmentKey(name)
	value := ""
	for _, item := range env {
		key, text, ok := strings.Cut(item, "=")
		if ok && canonicalEnvironmentKey(key) == want {
			value = text
		}
	}
	return value
}

func overlayEnvironment(base, over []string) []string {
	result := slices.Clone(base)
	index := make(map[string]int, len(result))
	for i, item := range result {
		name, _, _ := strings.Cut(item, "=")
		index[canonicalEnvironmentKey(name)] = i
	}
	for _, item := range over {
		name, _, _ := strings.Cut(item, "=")
		key := canonicalEnvironmentKey(name)
		if at, ok := index[key]; ok {
			result[at] = item
			continue
		}
		index[key] = len(result)
		result = append(result, item)
	}
	return result
}
