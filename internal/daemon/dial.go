package daemon

import (
	"context"
	"errors"
	"net"

	"github.com/gosuda/dalgona/internal/rpc"
)

func DialHome(home string) (net.Conn, error) {
	if err := checkSocketPathLimit(home); err != nil {
		return nil, err
	}
	sock := SocketPath(home)
	if _, err := socketOwned(sock); err != nil {
		return nil, err
	}
	return dialPeer(sock)
}

func Untrusted(err error) bool { return errors.Is(err, errUntrusted) }

func ProbeConn(ctx context.Context, conn net.Conn) (Info, Probe, error) {
	client := rpc.Dial(conn, daemonClient{})
	defer func() {
		_ = client.Close()
		_ = conn.Close()
	}()
	var info Info
	callErr := client.Call(ctx, MethodIdentity, nil, &info)
	if callErr == nil {
		return info, ProbeReady, nil
	}
	if rpcErr, notFound := errors.AsType[*rpc.RPCError](callErr); notFound && rpcErr.Code == methodNotFound {
		return info, ProbePreIdentity, nil
	}
	return info, ProbeAbsent, callErr
}
