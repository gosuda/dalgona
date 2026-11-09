package main

import (
	"errors"
	"io"
	"testing"
	"time"
)

type fakeBridge struct {
	fromClient *io.PipeReader
	toRemote   *io.PipeWriter
	fromRemote *io.PipeReader
	toClient   *io.PipeWriter
}

func newFakeBridge() *fakeBridge {
	fromClient, toRemote := io.Pipe()
	fromRemote, toClient := io.Pipe()
	return &fakeBridge{fromClient: fromClient, toRemote: toRemote, fromRemote: fromRemote, toClient: toClient}
}

func (b *fakeBridge) Read(p []byte) (int, error)  { return b.fromRemote.Read(p) }
func (b *fakeBridge) Write(p []byte) (int, error) { return b.toRemote.Write(p) }
func (b *fakeBridge) Close() error {
	_ = b.toRemote.Close()
	return b.fromRemote.Close()
}

type halfBridge struct{ *fakeBridge }

func (b halfBridge) CloseWrite() error { return b.toRemote.Close() }

func TestRemoteConnPassesClientEOFToTheBox(t *testing.T) {
	bridge := newFakeBridge()
	conn := pipeConn(halfBridge{bridge})
	half, ok := conn.(interface{ CloseWrite() error })
	if !ok {
		t.Fatal("remote conn over a half-closable stream has no CloseWrite")
	}
	remoteSaw := serveBox(bridge)
	if _, err := conn.Write([]byte("request")); err != nil {
		t.Fatal(err)
	}
	if err := half.CloseWrite(); err != nil {
		t.Fatal(err)
	}
	requireBoxRead(t, remoteSaw, "request")
	reply, err := io.ReadAll(conn)
	if err != nil || string(reply) != "reply" {
		t.Fatalf("client read %q, %v; want reply then EOF", reply, err)
	}
	if err := conn.Close(); err != nil {
		t.Fatal(err)
	}
}

type boxRead struct {
	data string
	err  error
}

func serveBox(bridge *fakeBridge) <-chan boxRead {
	remoteSaw := make(chan boxRead, 1)
	go func() {
		data, err := io.ReadAll(bridge.fromClient)
		if err == nil {
			_, err = bridge.toClient.Write([]byte("reply"))
		}
		remoteSaw <- boxRead{data: string(data), err: errors.Join(err, bridge.toClient.Close())}
	}()
	return remoteSaw
}

func requireBoxRead(t *testing.T, remoteSaw <-chan boxRead, want string) {
	t.Helper()
	select {
	case got := <-remoteSaw:
		if got.err != nil || got.data != want {
			t.Fatalf("box read %q, %v; want %q", got.data, got.err, want)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("box never saw EOF after the client closed its write half")
	}
}

func TestRemoteConnWithoutHalfCloseOffersNone(t *testing.T) {
	conn := pipeConn(newFakeBridge())
	defer func() { _ = conn.Close() }()
	if _, ok := conn.(interface{ CloseWrite() error }); ok {
		t.Fatal("remote conn claims CloseWrite over a stream that cannot half-close")
	}
}
