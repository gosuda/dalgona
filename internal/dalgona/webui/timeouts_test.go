package webui

import (
	"bufio"
	"context"
	"io"
	"net"
	"net/http"
	"os"
	"sync"
	"sync/atomic"
	"testing"
	"testing/synctest"
	"time"
)

type memAddr struct{}

func (memAddr) Network() string { return "mem" }
func (memAddr) String() string  { return "mem" }

type memListener struct {
	conns chan net.Conn
	done  chan struct{}
	once  sync.Once
}

func (l *memListener) Accept() (net.Conn, error) {
	select {
	case conn := <-l.conns:
		return conn, nil
	case <-l.done:
		return nil, net.ErrClosed
	}
}

func (l *memListener) Close() error {
	l.once.Do(func() { close(l.done) })
	return nil
}

func (l *memListener) Addr() net.Addr { return memAddr{} }

type watchedConn struct {
	net.Conn
	closedAt     atomic.Int64
	readExpiryAt atomic.Int64
}

func (c *watchedConn) Read(p []byte) (int, error) {
	n, err := c.Conn.Read(p)
	if err != nil && os.IsTimeout(err) {
		c.readExpiryAt.CompareAndSwap(0, time.Now().UnixNano())
	}
	return n, err
}

func (c *watchedConn) Close() error {
	c.closedAt.CompareAndSwap(0, time.Now().UnixNano())
	return c.Conn.Close()
}

func (c *watchedConn) closedAfter(start time.Time) time.Duration {
	return elapsedSince(start, c.closedAt.Load())
}

func (c *watchedConn) readExpiredAfter(start time.Time) time.Duration {
	return elapsedSince(start, c.readExpiryAt.Load())
}

func elapsedSince(start time.Time, at int64) time.Duration {
	if at == 0 {
		return -1
	}
	return time.Unix(0, at).Sub(start)
}

type memServer struct {
	listener *memListener
	clients  []net.Conn
}

func startMemServer(t *testing.T) *memServer {
	t.Helper()
	pages, err := pageHandler(builtSite())
	if err != nil {
		t.Fatalf("page handler: %v", err)
	}
	m := &memServer{listener: &memListener{conns: make(chan net.Conn, 1), done: make(chan struct{})}}
	s := &server{dial: refuseDial, log: io.Discard}
	ctx, cancel := context.WithCancel(t.Context())
	done := make(chan error, 1)
	go func() { done <- s.run(ctx, m.listener, pages) }()
	t.Cleanup(func() {
		for _, client := range m.clients {
			_ = client.Close()
		}
		cancel()
		if err := <-done; err != nil {
			t.Errorf("run after cancel: %v", err)
		}
	})
	return m
}

func (m *memServer) connect() (net.Conn, *watchedConn) {
	client, server := net.Pipe()
	watched := &watchedConn{Conn: server}
	m.clients = append(m.clients, client)
	m.listener.conns <- watched
	return client, watched
}

type timeoutCase struct {
	name     string
	request  string
	answered bool
	want     time.Duration
	observe  func(*watchedConn, time.Time) time.Duration
}

func exchange(t *testing.T, client net.Conn, request string, answered bool) {
	t.Helper()
	if _, err := io.WriteString(client, request); err != nil {
		t.Fatalf("write request: %v", err)
	}
	if !answered {
		return
	}
	response, err := http.ReadResponse(bufio.NewReader(client), nil)
	if err != nil {
		t.Fatalf("read response: %v", err)
	}
	if _, err := io.Copy(io.Discard, response.Body); err != nil {
		t.Fatalf("read body: %v", err)
	}
}

func (c timeoutCase) run(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		client, watched := startMemServer(t).connect()
		exchange(t, client, c.request, c.answered)
		begin := time.Now()
		time.Sleep(5 * time.Minute)
		synctest.Wait()
		got := c.observe(watched, begin)
		if got < 0 {
			t.Fatalf("connection stayed open, want it to end after %v", c.want)
		}
		if got != c.want {
			t.Fatalf("connection ended after %v, want %v", got, c.want)
		}
	})
}

func TestServerTimeoutsMatchSpec(t *testing.T) {
	const getRoot = "GET / HTTP/1.1\r\nHost: dalgona\r\n\r\n"
	cases := []timeoutCase{
		{
			"the HTTP server MUST set a 10 second header read timeout",
			"GET / HTTP/1.1\r\nHost: dalgona\r\n",
			false,
			10 * time.Second,
			(*watchedConn).closedAfter,
		},
		{
			"the HTTP server MUST set a 30 second read timeout",
			"POST / HTTP/1.1\r\nHost: dalgona\r\nContent-Length: 1\r\n\r\n",
			false,
			30 * time.Second,
			(*watchedConn).readExpiredAfter,
		},
		{
			"the HTTP server MUST set a 60 second write timeout",
			getRoot,
			false,
			60 * time.Second,
			(*watchedConn).closedAfter,
		},
		{
			"the HTTP server MUST set a 120 second idle timeout",
			getRoot,
			true,
			120 * time.Second,
			(*watchedConn).closedAfter,
		},
	}
	for _, c := range cases {
		t.Run(c.name, c.run)
	}
}
