package daemon

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"io"
	"net"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

type machineFixture struct {
	ctx     context.Context
	pairing *Pairing
	serve   func(context.Context, io.ReadWriteCloser) error
}

func (f machineFixture) dial() net.Conn {
	near, far := net.Pipe()
	go func() {
		defer func() { _ = far.Close() }()
		_ = ServeMachine(f.ctx, far, func() (*Pairing, error) { return f.pairing, nil }, f.serve)
	}()
	return near
}

func (f machineFixture) call(req rpc.MachineRequest) (rpc.MachineReply, error) {
	stream := f.dial()
	defer func() { _ = stream.Close() }()
	return rpc.ExchangeMachine(f.ctx, stream, req)
}

func machineCall(t *testing.T, f machineFixture, req rpc.MachineRequest) rpc.MachineReply {
	t.Helper()
	reply, err := f.call(req)
	if err != nil {
		t.Fatalf("%s: %v", req.Action, err)
	}
	return reply
}

func openMachineFixture(t *testing.T, home string) machineFixture {
	t.Helper()
	p, err := OpenPairing(home)
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(t.Context())
	t.Cleanup(cancel)
	return machineFixture{ctx: ctx, pairing: p}
}

func grantExchange(t *testing.T, f machineFixture) rpc.MachineRequest {
	t.Helper()
	grant := machineCall(t, f, rpc.MachineRequest{Action: rpc.MachineGrant})
	if grant.Status != "" || grant.Credential == "" {
		t.Fatalf("grant status %q, expected a code", grant.Status)
	}
	return rpc.MachineRequest{Action: rpc.MachineExchange, MachineID: grant.MachineID, Credential: grant.Credential, Label: "workstation"}
}

func TestPairingExpiryAndRestart(t *testing.T) {
	home := t.TempDir()
	f := openMachineFixture(t, home)
	var clock atomic.Int64
	f.pairing.now = func() time.Time { return time.Unix(clock.Load(), 0) }
	expiring := grantExchange(t, f)
	clock.Store(299)
	stillValid := grantExchange(t, f)
	clock.Store(300)
	if reply := machineCall(t, f, expiring); reply.Status != rpc.MachineDenied {
		t.Fatalf("grant at five-minute deadline: %q", reply.Status)
	}
	settled := machineCall(t, f, stillValid)
	if settled.Status != "" || settled.Device == nil {
		t.Fatalf("valid grant at one second old: %q", settled.Status)
	}
	unused := grantExchange(t, f)
	restarted := openMachineFixture(t, home)
	if got := machineCall(t, restarted, unused); got.MachineID != unused.MachineID || got.Status != rpc.MachineDenied {
		t.Fatalf("restart did not keep identity and discard unused grant: %q", got.Status)
	}
	listed := machineCall(t, restarted, rpc.MachineRequest{Action: rpc.MachineDevices})
	if len(listed.Devices) != 1 || listed.Devices[0].ID != settled.Device.ID {
		t.Fatal("restart lost the committed device")
	}
}

func TestPairingGrantsBindMachineAndExchangeOnce(t *testing.T) {
	f := openMachineFixture(t, t.TempDir())
	exchange := grantExchange(t, f)
	assertExchangeMismatch(t, f, exchange)
	wins, refusals := raceExchangeGrant(t, f, exchange)
	if wins != 1 || refusals != 7 {
		t.Fatalf("same grant: %d successes, %d refusals", wins, refusals)
	}
}

func assertExchangeMismatch(t *testing.T, f machineFixture, exchange rpc.MachineRequest) {
	t.Helper()
	wrong := exchange
	wrong.MachineID = "replacement-home"
	if reply := machineCall(t, f, wrong); reply.Status != rpc.MachineMismatch {
		t.Fatalf("wrong machine status %q", reply.Status)
	}
}

func raceExchangeGrant(t *testing.T, f machineFixture, exchange rpc.MachineRequest) (int32, int32) {
	t.Helper()
	var wg sync.WaitGroup
	start := make(chan struct{})
	var wins, refusals atomic.Int32
	for range 8 {
		wg.Go(func() {
			<-start
			tallyExchangeAttempt(t, f, exchange, &wins, &refusals)
		})
	}
	close(start)
	wg.Wait()
	return wins.Load(), refusals.Load()
}

func tallyExchangeAttempt(t *testing.T, f machineFixture, exchange rpc.MachineRequest, wins, refusals *atomic.Int32) {
	t.Helper()
	reply, err := f.call(exchange)
	switch {
	case err != nil:
		t.Error(err)
	case isExchangeSuccess(reply):
		wins.Add(1)
	case reply.Status == rpc.MachineDenied:
		refusals.Add(1)
	default:
		t.Errorf("unexpected exchange status %q", reply.Status)
	}
}

func isExchangeSuccess(reply rpc.MachineReply) bool {
	return reply.Status == "" && reply.Device != nil && reply.Credential != ""
}

func TestPairingRevokeClosesAdmittedStreams(t *testing.T) {
	home := t.TempDir()
	f := openMachineFixture(t, home)
	start, canceled := make(chan struct{}), make(chan struct{})
	f.serve = waitingDevice(start, canceled)
	paired := machineCall(t, f, grantExchange(t, f))
	if paired.Device == nil {
		t.Fatalf("no paired device: %q", paired.Status)
	}
	admit := rpc.MachineRequest{Action: rpc.MachineAdmit, MachineID: paired.MachineID, Credential: paired.Credential}
	conn := f.dial()
	defer func() { _ = conn.Close() }()
	if reply, err := rpc.ExchangeMachine(t.Context(), conn, admit); err != nil || reply.Status != "" {
		t.Fatalf("admit paired device: %q, %v", reply.Status, err)
	}
	peer := rpc.Dial(t.Context(), conn, nil)
	defer func() { _ = peer.Close() }()
	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
	defer cancel()
	callErr := make(chan error, 1)
	go func() { callErr <- peer.Call(ctx, "hold", nil, nil) }()
	waitDevice(t, ctx, start)
	revoke := rpc.MachineRequest{Action: rpc.MachineRevoke, DeviceID: paired.Device.ID}
	revokeAlongsideAdmissions(t, f, admit, revoke)
	waitDevice(t, ctx, canceled)
	if err := <-callErr; !errors.Is(err, io.ErrClosedPipe) {
		t.Fatalf("revoked call remained usable: %v", err)
	}
	for _, fixture := range []machineFixture{f, openMachineFixture(t, home)} {
		if reply := machineCall(t, fixture, admit); reply.Status != rpc.MachineDenied {
			t.Fatalf("revoked credential admitted: %q", reply.Status)
		}
	}
}

func revokeAlongsideAdmissions(t *testing.T, f machineFixture, admit, revoke rpc.MachineRequest) {
	t.Helper()
	var wg sync.WaitGroup
	defer wg.Wait()
	start, revoked := make(chan struct{}), make(chan struct{})
	defer close(revoked)
	for range 4 {
		wg.Go(func() {
			conn := f.dial()
			defer func() { _ = conn.Close() }()
			<-start
			reply, err := rpc.ExchangeMachine(f.ctx, conn, admit)
			if err == nil && reply.Status != "" && reply.Status != rpc.MachineDenied {
				t.Errorf("racing admission returned unexpected status: %q", reply.Status)
			}
			<-revoked
			assertRacingStreamClosed(t, conn)
		})
	}
	close(start)
	if reply := machineCall(t, f, revoke); reply.Status != "" {
		t.Fatalf("revoke failed: %q", reply.Status)
	}
}

func assertRacingStreamClosed(t *testing.T, conn net.Conn) {
	t.Helper()
	_ = conn.SetReadDeadline(time.Now().Add(2 * time.Second))
	var received [1]byte
	if _, err := conn.Read(received[:]); !errors.Is(err, io.EOF) && !errors.Is(err, io.ErrClosedPipe) && !errors.Is(err, net.ErrClosed) {
		t.Errorf("admission raced past revocation: %v", err)
	}
}

func waitingDevice(start, canceled chan struct{}) func(context.Context, io.ReadWriteCloser) error {
	return func(ctx context.Context, stream io.ReadWriteCloser) error {
		peer := rpc.NewConn(ctx, stream, func(ctx context.Context, _ string, _ jsontext.Value) (jsontext.Value, error) {
			close(start)
			<-ctx.Done()
			close(canceled)
			return nil, ctx.Err()
		})
		defer func() { _ = peer.Close() }()
		select {
		case <-ctx.Done():
		case <-peer.Done():
		}
		return nil
	}
}

func waitDevice(t *testing.T, ctx context.Context, event <-chan struct{}) {
	t.Helper()
	select {
	case <-ctx.Done():
		t.Fatal(ctx.Err())
	case <-event:
	}
}
