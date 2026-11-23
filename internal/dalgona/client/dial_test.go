package client

import (
	"errors"
	"io/fs"
	"os"
	"strings"
	"testing"
)

func TestDialHomeWithoutSocketIsAbsentAndNotUntrusted(t *testing.T) {
	home := shortHome(t)
	conn, err := DialHome(home)
	if err == nil {
		_ = conn.Close()
		t.Fatal("DialHome connected to a home with no socket")
	}
	if !errors.Is(err, fs.ErrNotExist) || Untrusted(err) {
		t.Fatalf("DialHome error = %v, want an absent socket that is not untrusted", err)
	}
	if _, statErr := os.Lstat(SocketPath(home)); !errors.Is(statErr, fs.ErrNotExist) {
		t.Fatalf("DialHome created a socket file: %v", statErr)
	}
}

func TestDialHomeRefusesAPathOverTheSocketLimit(t *testing.T) {
	home := shortHome(t) + strings.Repeat("d", socketPathLimit)
	conn, err := DialHome(home)
	if err == nil {
		_ = conn.Close()
		t.Fatal("DialHome connected through a path over the socket limit")
	}
	if !strings.Contains(err.Error(), "use a shorter home directory") {
		t.Fatalf("DialHome error = %v, want the socket path limit error", err)
	}
	if errors.Is(err, fs.ErrNotExist) || Untrusted(err) {
		t.Fatalf("DialHome error = %v, want neither an absent nor an untrusted socket", err)
	}
}
