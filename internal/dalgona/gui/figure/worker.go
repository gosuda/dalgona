package figure

import (
	"bytes"
	"crypto/rand"
	"encoding/hex"
	"io"
	"os"
	"os/exec"
	"sync"
	"time"
)

const (
	stderrLimit  = 4096
	helloTimeout = 60 * time.Second
)

type frame struct {
	env      envelope
	payload  []byte
	err      error
	borrowed bool
}

type workerProcess struct {
	cmd      *exec.Cmd
	stdin    io.WriteCloser
	frames   chan frame
	drained  chan struct{}
	stop     chan struct{}
	exited   chan struct{}
	stopOnce sync.Once
	stderr   *boundedBuffer
	warm     map[string]bool
}

type boundedBuffer struct {
	mu  sync.Mutex
	buf bytes.Buffer
}

func (b *boundedBuffer) Write(p []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	if room := stderrLimit - b.buf.Len(); room > 0 {
		b.buf.Write(p[:min(len(p), room)])
	}
	return len(p), nil
}

func (b *boundedBuffer) String() string {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.String()
}

func childEnvironment() []string {
	env := make([]string, 0, 1)
	if v, ok := os.LookupEnv("SystemRoot"); ok {
		env = append(env, "SystemRoot="+v)
	}
	return env
}

func startWorker(executable string, args []string) (*workerProcess, error) {
	cmd := exec.Command(executable, args...)
	cmd.Env = childEnvironment()
	cmd.Dir = os.TempDir()
	stdin, err := cmd.StdinPipe()
	if err != nil {
		return nil, fail(FailureEngine, "figure renderer pipes failed: %v", err)
	}
	stdout, childOut, err := os.Pipe()
	if err != nil {
		return nil, fail(FailureEngine, "figure renderer pipes failed: %v", err)
	}
	cmd.Stdout = childOut
	w := &workerProcess{cmd: cmd, stdin: stdin, frames: make(chan frame), drained: make(chan struct{}), stop: make(chan struct{}), exited: make(chan struct{}), stderr: &boundedBuffer{}, warm: map[string]bool{}}
	cmd.Stderr = w.stderr
	err = cmd.Start()
	_ = childOut.Close()
	if err != nil {
		_ = stdout.Close()
		return nil, fail(FailureEngine, "figure renderer did not start: %v", err)
	}
	go w.read(stdout)
	go func() {
		_ = cmd.Wait()
		close(w.exited)
	}()
	return w, nil
}

func (w *workerProcess) read(stdout *os.File) {
	defer stdout.Close()
	var scratch []byte
	for {
		env, payload, err := readFrameInto(stdout, ReplyFrameLimit, &scratch)
		borrowed := err == nil && clipboardChunk(env)
		select {
		case w.frames <- frame{env: env, payload: payload, err: err, borrowed: borrowed}:
		case <-w.stop:
			return
		}
		if err != nil {
			return
		}
		if borrowed {
			select {
			case <-w.drained:
			case <-w.stop:
				return
			}
		}
	}
}

func (w *workerProcess) collect(c *collector, f frame) (bool, bool) {
	done, healthy := c.accept(f)
	if f.borrowed {
		select {
		case w.drained <- struct{}{}:
		case <-w.stop:
		}
	}
	return done, healthy
}

func (w *workerProcess) handshake(timeout time.Duration, fonts string) error {
	var raw [32]byte
	if _, err := rand.Read(raw[:]); err != nil {
		return fail(FailureEngine, "figure renderer nonce failed: %v", err)
	}
	nonce := hex.EncodeToString(raw[:])
	if err := writeFrame(w.stdin, &envelope{Type: messageLaunch, Version: ProtocolVersion, Nonce: nonce}, nil, frameHeader+EnvelopeLimit); err != nil {
		return err
	}
	timer := time.NewTimer(timeout)
	defer timer.Stop()
	select {
	case f := <-w.frames:
		return checkHello(f, nonce, fonts)
	case <-w.exited:
		return fail(FailureEngine, "figure renderer exited before its hello: %s", firstLine(w.stderr.String()))
	case <-timer.C:
		return fail(FailureTimeout, "figure renderer sent no hello within %s", timeout)
	}
}

func checkHello(f frame, nonce, fonts string) error {
	switch {
	case f.err != nil:
		return f.err
	case f.env.Type == messageError:
		return failureFromEnvelope(f.env)
	case f.env.Type != messageHello || f.env.Version != ProtocolVersion:
		return fail(FailureProtocol, "figure renderer protocol version %d does not match %d", f.env.Version, ProtocolVersion)
	case f.env.Engines != engineManifest() || f.env.Fonts != fonts:
		return fail(FailureProtocol, "figure renderer engine or font manifest does not match this application")
	case f.env.Nonce != nonce:
		return fail(FailureProtocol, "figure renderer answered with another launch nonce")
	}
	return nil
}

func (w *workerProcess) terminate() {
	w.stopOnce.Do(func() {
		close(w.stop)
		_ = w.cmd.Process.Kill()
		_ = w.stdin.Close()
	})
	<-w.exited
}

func (w *workerProcess) alive() bool {
	select {
	case <-w.exited:
		return false
	default:
		return true
	}
}
