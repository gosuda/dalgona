package runner

import (
	"bufio"
	"errors"
	"net"
	"sync"
	"testing"
	"testing/synctest"
	"time"
)

func serveStallingTerminal(conn net.Conn, failing func() bool) {
	reader := bufio.NewReader(conn)
	writer := rpcWriter{w: conn, mu: &sync.Mutex{}}
	delivered := false
	for {
		line, err := readLimitedLine(reader, maxRPCLine)
		if err != nil {
			return
		}
		msg, err := parseRPC(line)
		if err != nil || !msg.hasID {
			continue
		}
		serveStallMessage(writer, msg, failing, &delivered)
	}
}

func serveStallMessage(writer rpcWriter, msg rpcMessage, failing func() bool, delivered *bool) {
	switch {
	case msg.method == "initialize":
		result, _ := encodeInitialize()
		_ = writeRPCResult(writer, msg.id, result)
	case msg.method == "terminal/open":
		result, _ := encodeTerminalID("stalling")
		_ = writeRPCResult(writer, msg.id, result)
	case msg.method == "terminal/read" && failing():
		_ = writeRPCError(writer, msg.id, rpcInternal, "exec server stalled")
	case msg.method == "terminal/read" && !*delivered:
		*delivered = true
		result, _ := encodeOutput(Output{Data: []byte("back"), Next: 4, Alive: true})
		_ = writeRPCResult(writer, msg.id, result)
	case msg.method == "terminal/read":
		result, _ := encodeOutput(Output{Next: 4, Alive: true})
		_ = writeRPCResult(writer, msg.id, result)
	default:
		_ = writeRPCResult(writer, msg.id, emptyObject())
	}
}

type stallRead struct {
	data    string
	elapsed time.Duration
	err     error
	term    Terminal
}

func readThroughStall(t *testing.T, stall time.Duration) stallRead {
	t.Helper()
	client, server := net.Pipe()
	recovers := time.Now().Add(stall)
	go serveStallingTerminal(server, func() bool { return time.Now().Before(recovers) })
	t.Cleanup(func() { _ = server.Close() })
	remote, err := NewRemote(t.Context(), "stall-test", client)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = remote.Close() })
	term, err := remote.OpenTerminal(t.Context(), TerminalSpec{Dir: "/work", Columns: 80, Rows: 24})
	if err != nil {
		t.Fatal(err)
	}
	started := time.Now()
	chunk := make([]byte, 16)
	n, readErr := term.Read(chunk)
	return stallRead{data: string(chunk[:n]), elapsed: time.Since(started), err: readErr, term: term}
}

func TestRemoteTerminalReadSurvivesAStallShorterThanTheLease(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		got := readThroughStall(t, 29*time.Second)
		data, elapsed, err, term := got.data, got.elapsed, got.err, got.term
		if err != nil || data != "back" || elapsed < 29*time.Second {
			t.Fatalf("read after a 29s stall = %q after %v, %v; want the retained bytes", data, elapsed, err)
		}
		select {
		case <-term.Done():
			t.Fatal("a recovered terminal reported an exit")
		default:
		}
	})
}

func TestRemoteTerminalReadMarksTheTerminalLostAfterTheLease(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		got := readThroughStall(t, 31*time.Second)
		data, elapsed, err, term := got.data, got.elapsed, got.err, got.term
		if !errors.Is(err, ErrTerminalLost) || data != "" || elapsed < remoteTerminalStall || elapsed >= 31*time.Second {
			t.Fatalf("read after a 31s stall = %q after %v, %v; want ErrTerminalLost at the 30s lease", data, elapsed, err)
		}
		select {
		case <-term.Done():
		default:
			t.Fatal("a lost terminal did not report its end")
		}
	})
}
