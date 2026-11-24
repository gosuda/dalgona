package tuiwire

import (
	"bytes"
	"context"
	"errors"
	"io"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/app"
	"github.com/gosuda/dalgona/internal/dal/config"
)

const guard = 5 * time.Second

type touchedConn struct {
	dialed  atomic.Int32
	touched atomic.Int32
	closed  atomic.Int32
	first   chan struct{}
	once    sync.Once
}

func (c *touchedConn) dial(context.Context) (io.ReadWriteCloser, error) {
	c.dialed.Add(1)
	return c, nil
}

func (c *touchedConn) touch() {
	c.touched.Add(1)
	c.once.Do(func() {
		if c.first != nil {
			close(c.first)
		}
	})
}

func (c *touchedConn) Read([]byte) (int, error) {
	c.touch()
	return 0, io.EOF
}

func (c *touchedConn) Write(p []byte) (int, error) {
	c.touch()
	return len(p), nil
}

func (c *touchedConn) Close() error {
	c.closed.Add(1)
	return nil
}

func assertRefused(t *testing.T, stdout io.Writer, written func() int) {
	t.Helper()
	conn := &touchedConn{}
	ctx, cancel := context.WithTimeout(t.Context(), guard)
	defer cancel()
	err := Local(conn.dial)(ctx, app.TUIOptions{Stdin: strings.NewReader(""), Stdout: stdout})
	if err == nil {
		t.Fatal("Run returned nil for a non-terminal stdout, want the refusal")
	}
	if !errors.Is(err, errNoTerminal) {
		t.Fatalf("Run error = %v, want %v", err, errNoTerminal)
	}
	if n := written(); n != 0 {
		t.Fatalf("Run wrote %d bytes to a non-terminal stdout before refusing, want 0", n)
	}
	if n := conn.touched.Load(); n != 0 {
		t.Fatalf("Run touched the connection %d times before refusing, want 0", n)
	}
	if n := conn.dialed.Load(); n != 0 {
		t.Fatalf("Run dialed %d times before refusing, want 0", n)
	}
}

func TestRunRefusesNonTerminalOutput(t *testing.T) {
	t.Run("buffer", func(t *testing.T) {
		var buffer bytes.Buffer
		assertRefused(t, &buffer, buffer.Len)
	})
	t.Run("regular file", func(t *testing.T) {
		file, err := os.Create(filepath.Join(t.TempDir(), "out.txt"))
		if err != nil {
			t.Fatal(err)
		}
		t.Cleanup(func() { _ = file.Close() })
		assertRefused(t, file, func() int {
			info, statErr := file.Stat()
			if statErr != nil {
				t.Fatal(statErr)
			}
			return int(info.Size())
		})
	})
}

func TestRunRefusalExitsNonzeroWithOneStderrLine(t *testing.T) {
	conn := &touchedConn{}
	var stdout, stderr bytes.Buffer
	env := app.CLIEnv{
		Product: config.ProductDal,
		Stdin:   strings.NewReader(""),
		Stdout:  &stdout,
		Stderr:  &stderr,
		TUI:     Local(conn.dial),
	}
	ctx, cancel := context.WithTimeout(t.Context(), guard)
	defer cancel()
	if code := app.RunCLI(ctx, env, nil); code != 1 {
		t.Fatalf("exit code = %d, want 1; stderr %q", code, stderr.String())
	}
	if got, want := stderr.String(), "dal: "+errNoTerminal.Error()+"\n"; got != want {
		t.Fatalf("stderr = %q, want %q", got, want)
	}
	if stdout.Len() != 0 {
		t.Fatalf("stdout = %q, want nothing", stdout.String())
	}
	if n := conn.touched.Load(); n != 0 {
		t.Fatalf("connection touched %d times, want 0", n)
	}
	if n := conn.dialed.Load(); n != 0 {
		t.Fatalf("TUI dialed %d times before refusing, want 0", n)
	}
}
