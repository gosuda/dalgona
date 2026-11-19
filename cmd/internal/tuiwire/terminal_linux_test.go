//go:build linux

package tuiwire

import (
	"context"
	"fmt"
	"io"
	"os"
	"strings"
	"testing"
	"time"

	"golang.org/x/sys/unix"

	"github.com/gosuda/dalgona/internal/dal/app"
)

func openPty(t *testing.T) *os.File {
	t.Helper()
	master, err := os.OpenFile("/dev/ptmx", os.O_RDWR|unix.O_NOCTTY, 0)
	if err != nil {
		t.Fatalf("open a pseudo-terminal: %v", err)
	}
	t.Cleanup(func() { _ = master.Close() })
	go func() { _, _ = io.Copy(io.Discard, master) }()
	fd := int(master.Fd())
	if err = unix.IoctlSetPointerInt(fd, unix.TIOCSPTLCK, 0); err != nil {
		t.Fatal(err)
	}
	number, err := unix.IoctlGetInt(fd, unix.TIOCGPTN)
	if err != nil {
		t.Fatal(err)
	}
	slave, err := os.OpenFile(fmt.Sprintf("/dev/pts/%d", number), os.O_RDWR|unix.O_NOCTTY, 0)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = slave.Close() })
	if err = unix.IoctlSetWinsize(int(slave.Fd()), unix.TIOCSWINSZ, &unix.Winsize{Row: 30, Col: 100}); err != nil {
		t.Fatal(err)
	}
	return slave
}

func TestIsTerminalAcceptsAPseudoTerminal(t *testing.T) {
	if !isTerminal(openPty(t)) {
		t.Fatal("isTerminal rejected a pseudo-terminal, want it accepted")
	}
}

func TestRunStartsOnTerminalStdout(t *testing.T) {
	conn := &touchedConn{first: make(chan struct{})}
	stdout := openPty(t)
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	done := make(chan error, 1)
	go func() {
		done <- Run(ctx, conn, app.TUIOptions{Stdin: strings.NewReader(""), Stdout: stdout})
	}()
	select {
	case <-conn.first:
	case err := <-done:
		t.Fatalf("Run returned %v before it used the connection, want the interface started on a terminal stdout", err)
	case <-time.After(guard):
		t.Fatal("Run did not use the connection on a terminal stdout")
	}
	cancel()
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("Run returned %v after ctx ended, want nil", err)
		}
	case <-time.After(guard):
		t.Fatal("Run did not return after ctx ended")
	}
}
