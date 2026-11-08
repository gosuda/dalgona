package mcp

import (
	"bufio"
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"sync"
	"time"
)

var errServerCrashed = errors.New("stdio server crashed")

type stdioTransport struct {
	command string
	args    []string
	env     []string
	cmd     *exec.Cmd
	stdin   io.WriteCloser
	stdout  *bufio.Reader
	stderr  stderrTail
}

const stderrTailBytes = 4096

type stderrTail struct {
	mu  sync.Mutex
	buf []byte
}

func (t *stderrTail) Write(p []byte) (int, error) {
	t.mu.Lock()
	defer t.mu.Unlock()
	if len(p) >= stderrTailBytes {
		t.buf = append(t.buf[:0], p[len(p)-stderrTailBytes:]...)
		return len(p), nil
	}
	t.buf = append(t.buf, p...)
	if excess := len(t.buf) - stderrTailBytes; excess > 0 {
		copy(t.buf, t.buf[excess:])
		t.buf = t.buf[:stderrTailBytes]
	}
	return len(p), nil
}

func (t *stderrTail) tail() string {
	t.mu.Lock()
	defer t.mu.Unlock()
	return string(t.buf)
}

type lineResult struct {
	line []byte
	err  error
}

type writeResult struct {
	n   int
	err error
}

func newStdioTransport(command string, args, env []string) (*stdioTransport, error) {
	transport := &stdioTransport{command: command, args: args, env: env}
	if err := transport.start(); err != nil {
		return nil, err
	}
	return transport, nil
}

func (t *stdioTransport) start() error {
	cmd := exec.Command(t.command, t.args...)
	cmd.Env = append(os.Environ(), t.env...)
	stdin, err := cmd.StdinPipe()
	if err != nil {
		return fmt.Errorf("mcp: open stdio stdin: %w", err)
	}
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		_ = stdin.Close()
		return fmt.Errorf("mcp: open stdio stdout: %w", err)
	}
	cmd.Stderr = &t.stderr
	if err := cmd.Start(); err != nil {
		_ = stdin.Close()
		return fmt.Errorf("mcp: start stdio server %q: %w", t.command, err)
	}
	t.cmd = cmd
	t.stdin = stdin
	t.stdout = bufio.NewReaderSize(stdout, 64*1024)
	return nil
}

func (t *stdioTransport) request(ctx context.Context, id uint64, method string, params jsontext.Value) (jsontext.Value, error) {
	ctx, cancel := context.WithTimeout(ctx, 5*time.Minute)
	defer cancel()
	message, err := encodeMessage(&id, method, params)
	if err != nil {
		return nil, err
	}
	if err := t.writeLine(ctx, message); err != nil {
		return nil, err
	}
	for {
		line, err := t.readLine(ctx)
		if err != nil {
			return nil, err
		}
		matched, result, err := matchResponse(line, id)
		if err != nil {
			return nil, fmt.Errorf("decode stdio response: %w", err)
		}
		if matched {
			return result, nil
		}
	}
}

func (t *stdioTransport) notify(ctx context.Context, method string) error {
	ctx, cancel := context.WithTimeout(ctx, 5*time.Minute)
	defer cancel()
	message, err := encodeMessage(nil, method, nil)
	if err != nil {
		return err
	}
	return t.writeLine(ctx, message)
}

func (t *stdioTransport) writeLine(ctx context.Context, message []byte) error {
	if t.stdin == nil {
		return errServerCrashed
	}
	stdin := t.stdin
	line := make([]byte, len(message), len(message)+1)
	copy(line, message)
	line = append(line, '\n')
	resultChannel := make(chan writeResult, 1)
	go func() {
		n, err := stdin.Write(line)
		resultChannel <- writeResult{n: n, err: err}
	}()
	select {
	case result := <-resultChannel:
		if result.err != nil {
			_ = t.Close()
			return fmt.Errorf("%w: write: %w; server stderr: %q", errServerCrashed, result.err, t.stderr.tail())
		}
		if result.n != len(line) {
			_ = t.Close()
			return fmt.Errorf("%w: %w; server stderr: %q", errServerCrashed, io.ErrShortWrite, t.stderr.tail())
		}
		return nil
	case <-ctx.Done():
		_ = t.Close()
		<-resultChannel
		return ctx.Err()
	}
}

func (t *stdioTransport) readLine(ctx context.Context) ([]byte, error) {
	if t.stdout == nil {
		return nil, errServerCrashed
	}
	reader := t.stdout
	resultChannel := make(chan lineResult, 1)
	go func() {
		line, err := reader.ReadBytes('\n')
		resultChannel <- lineResult{line: line, err: err}
	}()
	select {
	case result := <-resultChannel:
		if result.err != nil {
			_ = t.Close()
			return nil, fmt.Errorf("%w: read: %w; server stderr: %q", errServerCrashed, result.err, t.stderr.tail())
		}
		return result.line, nil
	case <-ctx.Done():
		_ = t.Close()
		<-resultChannel
		return nil, ctx.Err()
	}
}

func (t *stdioTransport) Close() error {
	cmd := t.cmd
	stdin := t.stdin
	t.cmd = nil
	t.stdin = nil
	t.stdout = nil
	if cmd == nil {
		return nil
	}
	return errors.Join(closePipe(stdin), stopProcess(cmd))
}

func closePipe(pipe io.Closer) error {
	if pipe == nil {
		return nil
	}
	if err := pipe.Close(); err != nil && !errors.Is(err, os.ErrClosed) {
		return err
	}
	return nil
}

func stopProcess(cmd *exec.Cmd) error {
	var errs []error
	if cmd.Process != nil {
		if err := cmd.Process.Kill(); err != nil && !errors.Is(err, os.ErrProcessDone) {
			errs = append(errs, err)
		}
	}
	if err := cmd.Wait(); err != nil {
		if _, exited := errors.AsType[*exec.ExitError](err); !exited {
			errs = append(errs, err)
		}
	}
	return errors.Join(errs...)
}
