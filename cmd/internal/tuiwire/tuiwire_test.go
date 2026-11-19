package tuiwire

import (
	"bytes"
	"context"
	"io"
	"os"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/app"
	"github.com/gosuda/dalgona/internal/dal/config"
)

const guard = 5 * time.Second

const refusal = "the interactive interface needs a terminal on stdout; use -p <prompt> for one turn"

type touchedConn struct {
	touched atomic.Int32
	closed  atomic.Int32
}

func (c *touchedConn) Read([]byte) (int, error) {
	c.touched.Add(1)
	return 0, io.EOF
}

func (c *touchedConn) Write(p []byte) (int, error) {
	c.touched.Add(1)
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
	err := Run(ctx, conn, app.TUIOptions{Stdin: strings.NewReader(""), Stdout: stdout})
	if err == nil {
		t.Fatal("Run returned nil for a non-terminal stdout, want the refusal")
	}
	if err.Error() != refusal {
		t.Fatalf("Run error = %q, want %q", err, refusal)
	}
	if !strings.Contains(err.Error(), "terminal on stdout") || !strings.Contains(err.Error(), "-p <prompt>") {
		t.Fatalf("Run error %q must name the cause and -p", err)
	}
	if n := written(); n != 0 {
		t.Fatalf("Run wrote %d bytes to a non-terminal stdout before refusing, want 0", n)
	}
	if n := conn.touched.Load(); n != 0 {
		t.Fatalf("Run touched the connection %d times before refusing, want 0", n)
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
		Dial:    func(context.Context) (io.ReadWriteCloser, error) { return conn, nil },
		Stdin:   strings.NewReader(""),
		Stdout:  &stdout,
		Stderr:  &stderr,
		TUI:     Run,
	}
	ctx, cancel := context.WithTimeout(t.Context(), guard)
	defer cancel()
	if code := app.RunCLI(ctx, env, nil); code != 1 {
		t.Fatalf("exit code = %d, want 1; stderr %q", code, stderr.String())
	}
	if got, want := stderr.String(), "dal: "+refusal+"\n"; got != want {
		t.Fatalf("stderr = %q, want %q", got, want)
	}
	if stdout.Len() != 0 {
		t.Fatalf("stdout = %q, want nothing", stdout.String())
	}
	if n := conn.touched.Load(); n != 0 {
		t.Fatalf("connection touched %d times, want 0", n)
	}
	if n := conn.closed.Load(); n != 1 {
		t.Fatalf("connection closed %d times, want 1", n)
	}
}
