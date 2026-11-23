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

func serveFakeDaemon(t *testing.T, sock string, handler rpc.Handler) net.Listener {
	t.Helper()
	listener, err := net.Listen("unix", sock)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.Chmod(sock, 0o600); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = listener.Close() })
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
	return listener
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

func TestRetireNeverReachesAReplacementDaemon(t *testing.T) {
	home := shortHome(t)
	if err := TightenHome(home); err != nil {
		t.Fatal(err)
	}
	sock := SocketPath(home)
	running := Info{Build: "00112233445566aa", PID: os.Getpid(), Version: "old", StartedAt: time.Unix(1_700_000_000, 0).UTC()}
	replacement := Info{Build: "00112233445566bb", PID: os.Getpid(), Version: "new", StartedAt: running.StartedAt.Add(time.Minute)}
	var oldRetired, replacementRetired atomic.Bool
	var old net.Listener
	var swapped atomic.Bool
	answer := fakeIdentity(t, running, func() { oldRetired.Store(true) })
	old = serveFakeDaemon(t, sock, func(ctx context.Context, method string, params jsontext.Value) (jsontext.Value, error) {
		if method == MethodIdentity && swapped.CompareAndSwap(false, true) {
			_ = old.Close()
			serveFakeDaemon(t, sock, fakeIdentity(t, replacement, func() { replacementRetired.Store(true) }))
		}
		return answer(ctx, method, params)
	})
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
	serveFakeDaemon(t, SocketPath(home), fakeIdentity(t, now, func() { retiredAny.Store(true) }))
	retired, err := Retire(t.Context(), home, seen)
	if _, ok := errors.AsType[*ReplacedError](err); !ok || retired {
		t.Fatalf("Retire = %v, %v; want ReplacedError", retired, err)
	}
	if retiredAny.Load() {
		t.Fatal("Retire retired a daemon whose start time changed")
	}
}
