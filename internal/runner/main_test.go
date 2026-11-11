package runner

import (
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"strings"
	"syscall"
	"testing"
	"time"
)

func TestMain(m *testing.M) {
	if len(os.Args) > 1 && os.Args[1] == "__shim" {
		os.Exit(ShimMain(os.Args[2:]))
	}
	if len(os.Args) > 1 && os.Args[1] == "__self-test" {
		_, _ = fmt.Fprint(os.Stdout, os.Args[0])
		os.Exit(0)
	}
	if handled, code := dispatchDialProbe(); handled {
		os.Exit(code)
	}
	if mode := os.Getenv("DALGONA_WINDOWS_SHIM_TEST_HELPER"); mode != "" {
		os.Exit(runWindowsShimHelper(mode))
	}
	os.Exit(m.Run())
}

func dispatchDialProbe() (bool, int) {
	if os.Getenv("DALGONA_TEST_DIAL") == "" {
		return false, 0
	}
	sock, err := dialTarget()
	if err != nil {
		_, _ = fmt.Fprintln(os.Stderr, "read target:", err)
		return true, 2
	}
	return true, dialProbe(sock)
}

func dialTarget() (string, error) {
	data, err := os.ReadFile("dial-target")
	if err != nil {
		return "", err
	}
	return strings.TrimSpace(string(data)), nil
}

func dialProbe(sock string) int {
	conn, err := net.DialTimeout("unix", sock, 10*time.Second)
	if err != nil {
		_, _ = fmt.Fprintln(os.Stderr, "dial:", err)
		if errors.Is(err, syscall.EPERM) || errors.Is(err, syscall.EACCES) {
			return 6
		}
		return 3
	}
	defer func() { _ = conn.Close() }()
	_ = conn.SetDeadline(time.Now().Add(10 * time.Second))
	if _, err := conn.Write([]byte("ping")); err != nil {
		_, _ = fmt.Fprintln(os.Stderr, "write:", err)
		return 4
	}
	buf := make([]byte, 2)
	if _, err := io.ReadFull(conn, buf); err != nil {
		_, _ = fmt.Fprintln(os.Stderr, "read:", err)
		return 4
	}
	if string(buf) != "ok" {
		_, _ = fmt.Fprintln(os.Stderr, "unexpected answer")
		return 5
	}
	return 0
}
