package daemon

import (
	"context"
	"errors"
	"io"
	"net"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func ServeMachine(ctx context.Context, conn net.Conn, open func() (*Pairing, error), serve func(context.Context, io.ReadWriteCloser) error) error {
	var first [1]byte
	if _, err := io.ReadFull(conn, first[:]); err != nil {
		if errors.Is(err, io.EOF) || errors.Is(err, net.ErrClosed) {
			return nil
		}
		return err
	}
	if first[0] != rpc.MachinePreface {
		return serve(ctx, &replayedConn{Conn: conn, first: first[0], pending: true})
	}
	return serveMachinePreface(ctx, conn, open, serve)
}

type replayedConn struct {
	net.Conn
	first   byte
	pending bool
}

func (c *replayedConn) Read(p []byte) (int, error) {
	if !c.pending || len(p) == 0 {
		return c.Conn.Read(p)
	}
	c.pending = false
	p[0] = c.first
	return 1, nil
}

func serveMachinePreface(ctx context.Context, conn net.Conn, open func() (*Pairing, error), serve func(context.Context, io.ReadWriteCloser) error) error {
	if err := conn.SetDeadline(time.Now().Add(5 * time.Second)); err != nil {
		return err
	}
	var req rpc.MachineRequest
	if err := rpc.ReadMachineFrame(conn, rpc.MachinePrefaceLimit, &req); err != nil || req.Version != rpc.MachineVersion {
		return rpc.WriteMachineFrame(conn, rpc.MachineReply{Version: rpc.MachineVersion, Status: rpc.MachineInvalid})
	}
	p, err := open()
	if err != nil {
		return rpc.WriteMachineFrame(conn, rpc.MachineReply{Version: rpc.MachineVersion, Status: rpc.MachineUnavailable})
	}
	switch req.Action {
	case rpc.MachineAdmit:
		return p.serveDevice(ctx, conn, req, serve)
	case rpc.MachineSSH:
		return p.serveSSH(ctx, conn, req.MachineID, serve)
	case rpc.MachineIdentity:
		return rpc.WriteMachineFrame(conn, p.identity(req.MachineID))
	default:
		return rpc.WriteMachineFrame(conn, p.control(req))
	}
}

func (p *Pairing) serveSSH(ctx context.Context, conn net.Conn, expected string, serve func(context.Context, io.ReadWriteCloser) error) error {
	reply := p.identity(expected)
	if err := rpc.WriteMachineFrame(conn, reply); err != nil || reply.Status != "" {
		return err
	}
	if err := conn.SetDeadline(time.Time{}); err != nil {
		return err
	}
	return serve(ctx, conn)
}

func (p *Pairing) serveDevice(ctx context.Context, conn net.Conn, req rpc.MachineRequest, serve func(context.Context, io.ReadWriteCloser) error) error {
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	stream := &deviceStream{conn: conn, cancel: cancel}
	device, status := p.admit(req, stream)
	if device != nil {
		defer p.release(device, stream)
	}
	if err := rpc.WriteMachineFrame(conn, p.reply(status)); err != nil || status != "" {
		return err
	}
	if err := conn.SetDeadline(time.Time{}); err != nil {
		return err
	}
	ctx = rpc.WithDeviceAdmission(ctx, func() bool { return !device.revoked.Load() })
	return serve(context.WithValue(ctx, pairedConnectionKey{}, true), conn)
}

type pairedConnectionKey struct{}

func PairedConnection(ctx context.Context) bool {
	paired, _ := ctx.Value(pairedConnectionKey{}).(bool)
	return paired
}
