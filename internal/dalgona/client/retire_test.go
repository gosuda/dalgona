//go:build !windows

package client

import (
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"net"
	"os"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func listenFakeDaemon(t *testing.T, sock string) (net.Listener, error) {
	listener, err := net.Listen("unix", sock)
	if err != nil {
		return nil, err
	}
	t.Cleanup(func() { _ = listener.Close() })
	return listener, os.Chmod(sock, 0o600)
}

func serveFakeDaemon(t *testing.T, listener net.Listener, handler rpc.Handler) {
	go func() {
		for {
			conn, err := listener.Accept()
			if err != nil {
				return
			}
			served := rpc.NewServerConn(t.Context(), conn, handler)
			served.Start()
			context.AfterFunc(t.Context(), func() { _ = served.Close() })
		}
	}()
}

func fakeIdentity(t *testing.T, info Info, retire func()) rpc.Handler {
	return func(_ context.Context, method string, _ jsontext.Value) (jsontext.Value, error) {
		switch method {
		case MethodIdentity:
			reply, err := json.Marshal(info)
			return jsontext.Value(reply), err
		case MethodRetire:
			retire()
			return jsontext.Value("{}"), nil
		default:
			t.Errorf("fake daemon got %s", method)
			return nil, &rpc.RPCError{Code: methodNotFound, Message: "method not found"}
		}
	}
}

func replaceAfterIdentity(t *testing.T, old net.Listener, sock string, answer, replacement rpc.Handler) rpc.Handler {
	var swapped atomic.Bool
	return func(ctx context.Context, method string, params jsontext.Value) (jsontext.Value, error) {
		if method == MethodIdentity && swapped.CompareAndSwap(false, true) {
			_ = old.Close()
			next, err := listenFakeDaemon(t, sock)
			if err != nil {
				t.Errorf("start the replacement daemon: %v", err)
				return nil, err
			}
			serveFakeDaemon(t, next, replacement)
		}
		return answer(ctx, method, params)
	}
}

func TestRetireNeverReachesAReplacementDaemon(t *testing.T) {
	home := shortHome(t)
	if err := TightenHome(home); err != nil {
		t.Fatal(err)
	}
	sock := SocketPath(home)
	running := Info{Build: "00112233445566aa", PID: os.Getpid(), Version: "old", StartedAt: time.Unix(1_700_000_000, 0).UTC()}
	replacement := Info{Build: "00112233445566bb", PID: os.Getpid(), Version: "new", StartedAt: running.StartedAt.Add(time.Minute)}
	var oldRetired, replacementRetired atomic.Bool
	old, err := listenFakeDaemon(t, sock)
	if err != nil {
		t.Fatal(err)
	}
	answer := fakeIdentity(t, running, func() { oldRetired.Store(true) })
	replacing := fakeIdentity(t, replacement, func() { replacementRetired.Store(true) })
	serveFakeDaemon(t, old, replaceAfterIdentity(t, old, sock, answer, replacing))
	ctx, cancel := context.WithTimeout(t.Context(), 500*time.Millisecond)
	defer cancel()
	retired, err := Retire(ctx, home, running)
	if replacementRetired.Load() {
		t.Fatal("Retire sent the retire request to the replacement daemon")
	}
	if !oldRetired.Load() {
		t.Fatalf("Retire = %v, %v; the identified daemon never got the retire request", retired, err)
	}
	if retired || err == nil {
		t.Fatalf("Retire = %v, %v; want an error while the replacement holds the socket", retired, err)
	}
}

func TestRetireRefusesADaemonThatChanged(t *testing.T) {
	home := shortHome(t)
	if err := TightenHome(home); err != nil {
		t.Fatal(err)
	}
	seen := Info{Build: "00112233445566aa", PID: os.Getpid(), StartedAt: time.Unix(1_700_000_000, 0).UTC()}
	now := seen
	now.StartedAt = seen.StartedAt.Add(time.Second)
	var retiredAny atomic.Bool
	listener, err := listenFakeDaemon(t, SocketPath(home))
	if err != nil {
		t.Fatal(err)
	}
	serveFakeDaemon(t, listener, fakeIdentity(t, now, func() { retiredAny.Store(true) }))
	retired, err := Retire(t.Context(), home, seen)
	if _, ok := errors.AsType[*ReplacedError](err); !ok || retired {
		t.Fatalf("Retire = %v, %v; want ReplacedError", retired, err)
	}
	if retiredAny.Load() {
		t.Fatal("Retire retired a daemon whose start time changed")
	}
}
