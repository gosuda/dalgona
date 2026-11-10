package exec

import (
	"bytes"
	"cmp"
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
)

type Tool struct {
	grace time.Duration
}

func New() *Tool { return &Tool{grace: 5 * time.Second} }

func (t *Tool) Spec() tool.Spec {
	return tool.Spec{
		Name:        "exec",
		Description: "Run a non-interactive shell command. Use escalate only after a sandbox denial. Declined calls return their reason so you can change course.",
		Schema:      jsontext.Value(`{"type":"object","properties":{"command":{"type":"string"},"cwd":{"type":"string"},"timeout_ms":{"type":"integer"},"escalate":{"type":"boolean"},"justification":{"type":"string"}},"required":["command"]}`),
		Keep:        tool.KeepTail,
	}
}

func (t *Tool) Class() tool.Class { return tool.ClassExec }

func (t *Tool) Run(ctx context.Context, call tool.Call, env tool.Env) (tool.Result, error) {
	args, err := parseArgs(call.Args)
	if err != nil {
		return tool.Result{}, err
	}
	if env.Runner == nil {
		return tool.Result{}, errors.New("exec: runner is required")
	}
	command := args.command
	if env.Masker != nil {
		unmasked, unmaskErr := env.Masker.UnmaskStrict(command)
		if unmaskErr != nil {
			return tool.Result{}, fmt.Errorf("exec: secret placeholder cannot be restored: %w", unmaskErr)
		}
		command = unmasked
	}
	mode := env.Sandbox.Mode
	if args.escalate {
		mode = runner.SandboxOff
	}
	cwd, err := commandDirectory(args.cwd, env, mode)
	if err != nil {
		return tool.Result{}, err
	}
	spec := runner.ProcessSpec{
		ID:      call.ID,
		Argv:    shellArgvFor(env.Runner, command),
		Dir:     cwd,
		Sandbox: runner.Sandbox{Mode: mode, WritableRoots: env.Sandbox.WritableRoots, DeniedSockets: env.Sandbox.DeniedSockets},
	}
	if err := env.Runner.Start(ctx, spec); err != nil {
		return tool.Result{}, fmt.Errorf("exec: start: %w", err)
	}
	return t.wait(ctx, env.Runner, call.ID, args.timeout)
}

type arguments struct {
	command  string
	cwd      string
	timeout  time.Duration
	escalate bool
}

func parseArgs(raw jsontext.Value) (arguments, error) {
	entries, err := readObject(raw)
	if err != nil {
		return arguments{}, err
	}
	result := arguments{timeout: 600 * time.Second}
	for _, entry := range entries {
		if err := setArgument(&result, entry); err != nil {
			return arguments{}, fmt.Errorf("exec: invalid %s: %w", entry.name, err)
		}
	}
	if result.command == "" {
		return arguments{}, errors.New("exec: command is required")
	}
	return result, nil
}

func setArgument(result *arguments, entry entry) error {
	value := bytes.TrimSpace(entry.value)
	switch entry.name {
	case "command":
		text, err := stringValue(value)
		result.command = text
		return err
	case "cwd":
		text, err := stringValue(value)
		result.cwd = text
		return err
	case "timeout_ms":
		return setTimeout(result, value)
	case "escalate":
		flag, err := parseBool(value)
		result.escalate = flag
		return err
	case "justification":
		_, err := stringValue(value)
		return err
	default:
		return nil
	}
}

func setTimeout(result *arguments, value []byte) error {
	milliseconds, err := strconv.ParseInt(string(value), 10, 64)
	if err != nil {
		return fmt.Errorf("parse timeout: %w", err)
	}
	if milliseconds <= 0 || milliseconds > (1<<63-1)/int64(time.Millisecond) {
		return errors.New("timeout_ms must be positive and representable")
	}
	result.timeout = time.Duration(milliseconds) * time.Millisecond
	return nil
}

func commandDirectory(argument string, env tool.Env, mode runner.SandboxMode) (string, error) {
	if argument == "" {
		return execBaseDir(env)
	}
	base, err := execBaseDir(env)
	if err != nil {
		return "", err
	}
	candidate := resolveExecDir(argument, base)
	if mode == "" {
		mode = runner.SandboxWorkspaceWrite
	}
	if mode != runner.SandboxWorkspaceWrite {
		return candidate, nil
	}
	if !insideWritableRoots(env, candidate) {
		return "", fmt.Errorf("exec: cwd %q is outside the writable roots", candidate)
	}
	return candidate, nil
}

func execBaseDir(env tool.Env) (string, error) {
	if env.Cwd != "" {
		if filepath.IsAbs(env.Cwd) {
			return env.Cwd, nil
		}
		cwd, err := os.Getwd()
		if err != nil {
			return "", fmt.Errorf("exec: working directory: %w", err)
		}
		return filepath.Join(cwd, env.Cwd), nil
	}
	cwd, err := os.Getwd()
	if err != nil {
		return "", fmt.Errorf("exec: working directory: %w", err)
	}
	return cwd, nil
}

func resolveExecDir(argument string, base string) string {
	if filepath.IsAbs(argument) {
		return filepath.Clean(argument)
	}
	return filepath.Join(base, argument)
}

func insideWritableRoots(env tool.Env, candidate string) bool {
	base, err := execBaseDir(env)
	if err != nil {
		return false
	}
	target := resolveExistingLink(candidate)
	for _, root := range writableRoots(env, base) {
		if withinRoot(root, target) {
			return true
		}
	}
	return false
}

func writableRoots(env tool.Env, base string) []string {
	roots := make([]string, 0, len(env.Sandbox.WritableRoots)+1)
	if env.Cwd != "" {
		roots = append(roots, resolveExistingLink(base))
	}
	for _, root := range env.Sandbox.WritableRoots {
		if !filepath.IsAbs(root) {
			root = filepath.Join(base, root)
		}
		roots = append(roots, resolveExistingLink(root))
	}
	return roots
}

func withinRoot(root, target string) bool {
	rel, err := filepath.Rel(root, target)
	return err == nil && rel != ".." && !strings.HasPrefix(rel, ".."+string(filepath.Separator)) && !filepath.IsAbs(rel)
}

func resolveExistingLink(path string) string {
	suffix := ""
	for current := path; ; current = filepath.Dir(current) {
		if real, err := filepath.EvalSymlinks(current); err == nil {
			return filepath.Join(real, suffix)
		}
		parent := filepath.Dir(current)
		if parent == current {
			return path
		}
		suffix = filepath.Join(filepath.Base(current), suffix)
	}
}

const execOutputKeep = 64 * 1024

type waitState struct {
	tail   []byte
	total  int64
	offset int64
	path   string
}

func (s *waitState) appendOutput(data []byte) {
	s.total += int64(len(data))
	keep := data
	if len(keep) > execOutputKeep {
		keep = keep[len(keep)-execOutputKeep:]
	}
	s.tail = append(s.tail, keep...)
	if len(s.tail) > execOutputKeep {
		copy(s.tail, s.tail[len(s.tail)-execOutputKeep:])
		s.tail = s.tail[:execOutputKeep]
	}
}

func projectBounded(tail []byte, total int64, spill string) (string, bool) {
	if total <= execOutputKeep {
		return string(tail), false
	}
	window := trimTailStart(tail)
	return string(window) + "\n[output truncated: " + strconv.FormatInt(total, 10) + " bytes total; full output: " + spill + "; page it with read offset/limit]", true
}

func trimTailStart(data []byte) []byte {
	index := 0
	for index < len(data) && index < utf8.UTFMax-1 && !utf8.RuneStart(data[index]) {
		index++
	}
	return data[index:]
}

func (t *Tool) wait(ctx context.Context, run runner.Runner, id string, timeout time.Duration) (tool.Result, error) {
	timeoutErr := fmt.Errorf("exec: timed out after %s", timeout)
	waitCtx, cancel := context.WithTimeoutCause(ctx, timeout, timeoutErr)
	defer cancel()
	state := waitState{}
	delay := 10 * time.Millisecond
	for {
		current, readErr := run.Read(waitCtx, id, state.offset)
		if readErr != nil {
			readErr = timeoutCauseOr(readErr, waitCtx, ctx, timeoutErr)
			return t.terminate(run, id, state, readErr)
		}
		state.appendOutput(current.Data)
		state.offset = current.Next
		state.path = current.Path
		if current.Exit != nil {
			return projectedResult(state, current.Exit), nil
		}
		sleepErr := sleep(waitCtx, delay)
		if sleepErr != nil {
			if cause := context.Cause(waitCtx); cause != nil {
				sleepErr = cause
			}
			return t.terminate(run, id, state, sleepErr)
		}
		delay = min(delay*2, 100*time.Millisecond)
	}
}

func timeoutCauseOr(readErr error, waitCtx context.Context, parent context.Context, timeoutErr error) error {
	if parent.Err() != nil {
		return readErr
	}
	if waitCtx.Err() == nil {
		return readErr
	}
	if !errors.Is(readErr, context.DeadlineExceeded) && !errors.Is(readErr, context.Canceled) {
		return readErr
	}
	cause := context.Cause(waitCtx)
	if cause == nil {
		return timeoutErr
	}
	return cause
}

func (t *Tool) terminate(run runner.Runner, id string, state waitState, cause error) (tool.Result, error) {
	termErr := signalProcess(run, id, runner.SignalTerm)
	current, waitErr := t.awaitExit(run, id, state.offset, termErr)
	if current.Path != "" {
		state.appendOutput(current.Data)
		state.offset = current.Next
		state.path = current.Path
	}
	result := projectedResult(state, current.Exit)
	result.OK = false
	return result, errors.Join(cause, termErr, waitErr)
}

func (t *Tool) awaitExit(run runner.Runner, id string, offset int64, termErr error) (runner.Output, error) {
	grace := cmp.Or(t.grace, 5*time.Second)
	deadline := time.Now().Add(grace)
	for {
		current, readErr := readOnce(run, id, offset)
		if readErr == nil && current.Exit != nil {
			return current, nil
		}
		if termErr == nil && time.Now().Before(deadline) {
			time.Sleep(20 * time.Millisecond)
			continue
		}
		killErr := signalProcess(run, id, runner.SignalKill)
		current, readErr = readOnce(run, id, offset)
		return current, errors.Join(killErr, readErr)
	}
}

func readOnce(run runner.Runner, id string, offset int64) (runner.Output, error) {
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	return run.Read(ctx, id, offset)
}

func signalProcess(run runner.Runner, id string, signal runner.Signal) error {
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	if err := run.Signal(ctx, id, signal); err != nil {
		return fmt.Errorf("exec: signal %d: %w", signal, err)
	}
	return nil
}

func projectedResult(state waitState, exit *runner.Exit) tool.Result {
	projection, _ := projectBounded(state.tail, state.total, state.path)
	result := tool.Result{Output: []byte(projection), OK: false, Path: state.path}
	if exit != nil {
		result.OK = exit.Code == 0
		result.ExitCode = new(int)
		*result.ExitCode = exit.Code
	}
	return result
}

func sleep(ctx context.Context, delay time.Duration) error {
	timer := time.NewTimer(delay)
	defer timer.Stop()
	select {
	case <-ctx.Done():
		return fmt.Errorf("exec: wait: %w", ctx.Err())
	case <-timer.C:
		return nil
	}
}

func shellArgvFor(run runner.Runner, command string) []string {
	return shellArgvForHost(run, runtime.GOOS, command)
}

func shellArgvForHost(run runner.Runner, hostGOOS, command string) []string {
	type remoteHost interface{ RemoteHost() bool }
	if host, ok := run.(remoteHost); ok && host.RemoteHost() {
		return []string{"sh", "-c", command}
	}
	if hostGOOS == "windows" {
		return runner.WindowsShellArgv(command)
	}
	return []string{"sh", "-c", command}
}

type entry struct {
	name  string
	value jsontext.Value
}

func readObject(raw jsontext.Value) ([]entry, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(raw))
	start, err := decoder.ReadToken()
	if err != nil {
		return nil, fmt.Errorf("exec: read arguments: %w", err)
	}
	if start.Kind() != '{' {
		return nil, errors.New("exec: arguments must be an object")
	}
	entries := make([]entry, 0, 6)
	for decoder.PeekKind() != '}' {
		name, err := decoder.ReadToken()
		if err != nil {
			return nil, fmt.Errorf("exec: read argument name: %w", err)
		}
		nameText := name.String()
		value, err := decoder.ReadValue()
		if err != nil {
			return nil, fmt.Errorf("exec: read argument value: %w", err)
		}
		entries = append(entries, entry{name: nameText, value: value.Clone()})
	}
	if _, err := decoder.ReadToken(); err != nil {
		return nil, fmt.Errorf("exec: read argument end: %w", err)
	}
	return entries, nil
}

func stringValue(value []byte) (string, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(value))
	token, err := decoder.ReadToken()
	if err != nil {
		return "", fmt.Errorf("read string: %w", err)
	}
	if token.Kind() != '"' {
		return "", errors.New("expected string")
	}
	return token.String(), nil
}

func parseBool(value []byte) (bool, error) {
	switch strings.TrimSpace(string(value)) {
	case "true":
		return true, nil
	case "false":
		return false, nil
	default:
		return false, errors.New("expected boolean")
	}
}

var _ tool.Tool = (*Tool)(nil)
