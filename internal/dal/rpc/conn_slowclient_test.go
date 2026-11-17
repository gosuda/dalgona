package rpc

import (
	"bytes"
	"errors"
	"io"
	"log/slog"
	"net"
	"strings"
	"testing"
)

type nameless struct {
	io.Reader
	io.Writer
	io.Closer
}

func floodSlowClient(t *testing.T, c *Conn) {
	t.Helper()
	for {
		err := c.Notify("turn/started", map[string]string{"turnId": "turn"})
		if err == nil {
			continue
		}
		if !errors.Is(err, io.ErrShortBuffer) {
			t.Fatalf("notify error=%v, want io.ErrShortBuffer", err)
		}
		return
	}
}

func TestSlowClientLogNamesRemoteAddr(t *testing.T) {
	var log bytes.Buffer
	previous := slog.Default()
	slog.SetDefault(slog.New(slog.NewTextHandler(&log, nil)))
	t.Cleanup(func() { slog.SetDefault(previous) })

	left, right := net.Pipe()
	c := NewConn(t.Context(), left, nil)
	closeRPCPair(t, c, right)
	floodSlowClient(t, c)
	out := log.String()
	if strings.Contains(out, "<addr>") {
		t.Fatalf("slow client log kept the placeholder: %s", out)
	}
	if !strings.Contains(out, "rpc: closing slow client pipe") {
		t.Fatalf("slow client log missing the remote address: %s", out)
	}
}

func TestSlowClientLogWithoutRemoteAddr(t *testing.T) {
	var log bytes.Buffer
	previous := slog.Default()
	slog.SetDefault(slog.New(slog.NewTextHandler(&log, nil)))
	t.Cleanup(func() { slog.SetDefault(previous) })

	left, right := net.Pipe()
	c := NewConn(t.Context(), nameless{Reader: left, Writer: left, Closer: left}, nil)
	closeRPCPair(t, c, right)
	floodSlowClient(t, c)
	out := log.String()
	if strings.Contains(out, "<addr>") {
		t.Fatalf("slow client log kept the placeholder: %s", out)
	}
	if !strings.Contains(out, "rpc: closing slow client unknown") {
		t.Fatalf("slow client log missing the fallback address: %s", out)
	}
}
