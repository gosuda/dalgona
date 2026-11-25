//go:build windows

package runner

import (
	"bytes"
	"io"
	"os"
	"regexp"
	"testing"
	"time"
)

func openConsoleTerminal(t *testing.T) Terminal {
	t.Helper()
	local, err := NewLocal(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	spec := TerminalSpec{Dir: t.TempDir(), Env: os.Environ(), Sandbox: Sandbox{Mode: SandboxOff}, Columns: 100, Rows: 30}
	term, err := local.OpenTerminal(t.Context(), spec)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = term.Close() })
	return term
}

func readConsoleUntil(t *testing.T, term Terminal, want *regexp.Regexp) {
	t.Helper()
	var seen bytes.Buffer
	chunk := make([]byte, 4096)
	deadline := time.AfterFunc(20*time.Second, func() { _ = term.Close() })
	defer deadline.Stop()
	for !want.MatchString(seen.String()) {
		n, err := term.Read(chunk)
		seen.Write(chunk[:n])
		if err != nil {
			t.Fatalf("console output ended before %q: %v; saw %q", want, err, seen.String())
		}
	}
}

func awaitConsoleExit(t *testing.T, term Terminal) Exit {
	t.Helper()
	drained := make(chan error, 1)
	go func() {
		_, err := io.Copy(io.Discard, term)
		drained <- err
	}()
	select {
	case <-term.Done():
	case <-time.After(20 * time.Second):
		t.Fatal("console shell did not report its exit")
	}
	select {
	case err := <-drained:
		if err != nil {
			t.Fatalf("drain console output: %v", err)
		}
	case <-time.After(20 * time.Second):
		t.Fatal("console output did not end after the shell exited")
	}
	return term.Exit()
}

func TestConsoleTerminalCarriesInputResizeOutputAndExit(t *testing.T) {
	term := openConsoleTerminal(t)
	if _, err := term.Write([]byte("set /a 6*7\r\n")); err != nil {
		t.Fatal(err)
	}
	readConsoleUntil(t, term, regexp.MustCompile(`42`))
	if err := term.Resize(120, 40); err != nil {
		t.Fatal(err)
	}
	if _, err := term.Write([]byte("mode con\r\n")); err != nil {
		t.Fatal(err)
	}
	readConsoleUntil(t, term, regexp.MustCompile(`Columns:\s*120`))
	if _, err := term.Write([]byte("exit 3\r\n")); err != nil {
		t.Fatal(err)
	}
	if exit := awaitConsoleExit(t, term); exit.Code != 3 {
		t.Fatalf("exit = %+v, want code 3", exit)
	}
}

func TestConsoleTerminalKillEndsTheJob(t *testing.T) {
	term := openConsoleTerminal(t)
	if _, err := term.Write([]byte("set /a 6*7 & ping -n 60 127.0.0.1 > nul\r\n")); err != nil {
		t.Fatal(err)
	}
	readConsoleUntil(t, term, regexp.MustCompile(`42`))
	if err := term.Signal(SignalKill); err != nil {
		t.Fatal(err)
	}
	if exit := awaitConsoleExit(t, term); exit.Code != int(jobExitCode(SignalKill)) {
		t.Fatalf("exit = %+v, want the job exit code %d", exit, jobExitCode(SignalKill))
	}
}
