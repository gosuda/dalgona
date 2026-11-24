package main

import (
	"context"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/sha256"
	"crypto/x509"
	"encoding/hex"
	"encoding/json/jsontext"
	"encoding/pem"
	"io"
	"math/big"
	"net"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/daemon"
	"github.com/gosuda/dalgona/internal/dalgona/webui"
)

func TestDevicePairingThroughTLSRelay(t *testing.T) {
	ctx, cancel := context.WithTimeout(t.Context(), 15*time.Second)
	defer cancel()
	pairing, err := daemon.OpenPairing(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	dial := pairedTestDial(pairing)
	certFile, keyFile, pin := pairingCertificate(t)
	origin := startPairingRelay(t, ctx, dial, certFile, keyFile)
	grant := pairControl(t, ctx, dial, rpc.MachineRequest{Action: rpc.MachineGrant})
	cfg := config.Config{Home: t.TempDir(), TLSCAFile: certFile}
	card := rpc.PairingCard{Origin: origin, MachineID: grant.MachineID, PublicKeyPin: pin}
	remotes := client.NewRemotes(cfg, nil)
	if _, err := remotes.PairDevice(ctx, "work", card, grant.Credential, "laptop"); err != nil {
		t.Fatalf("pair through TLS: %v", err)
	}
	pairingReconnectAndRevoke(t, ctx, cfg, dial)
}

func pairingReconnectAndRevoke(t *testing.T, ctx context.Context, cfg config.Config, dial webui.Dialer) {
	t.Helper()
	saved, _, err := config.LoadRemoteSnapshot(cfg.Home, "work")
	if err != nil {
		t.Fatal(err)
	}
	cfg.Remotes = map[string]config.RemoteSpec{"work": saved}
	remotes := client.NewRemotes(cfg, nil)
	observer := pairedTestObserver{phases: make(chan client.PhaseChange, 16)}
	set, err := client.NewEndpointSet(remotes, client.ConfiguredEndpoints(cfg, client.Backend{}, remotes), observer, client.ProtocolTUI)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = set.Close() }()
	if err := set.Connect("work"); err != nil {
		t.Fatalf("connect using persisted device credential: %v", err)
	}
	observer.waitPhase(t, ctx, client.PhaseConnected)
	devices := pairControl(t, ctx, dial, rpc.MachineRequest{Action: rpc.MachineDevices}).Devices
	if len(devices) != 1 || devices[0].Label != "laptop" {
		t.Fatal("daemon did not record the exchanged device")
	}
	_ = pairControl(t, ctx, dial, rpc.MachineRequest{Action: rpc.MachineRevoke, DeviceID: devices[0].ID})
	change := observer.waitPhase(t, ctx, client.PhaseFailed)
	if change.Err == nil || !strings.Contains(change.Err.Error(), "refused the pairing grant or device credential") {
		t.Fatalf("revoked reconnect did not reach credential refusal: %v", change.Err)
	}
}

type pairedTestObserver struct{ phases chan client.PhaseChange }

func (o pairedTestObserver) PhaseChanged(change client.PhaseChange)     { o.phases <- change }
func (pairedTestObserver) Notified(client.Link, string, jsontext.Value) {}
func (pairedTestObserver) Arrived(client.Link, string, jsontext.Value)  {}
func (pairedTestObserver) Requested(context.Context, client.Link, string, jsontext.Value) (any, error) {
	return nil, &rpc.RPCError{Code: -32601, Message: "Method not found"}
}

func (o pairedTestObserver) waitPhase(t *testing.T, ctx context.Context, phase client.Phase) client.PhaseChange {
	t.Helper()
	var last client.PhaseChange
	for last.Phase != phase {
		select {
		case last = <-o.phases:
		case <-ctx.Done():
			t.Fatalf("wanted pairing phase %s; last was %s (%v)", phase, last.Phase, last.Err)
		}
	}
	return last
}

func pairedTestDial(pairing *daemon.Pairing) webui.Dialer {
	return func(ctx context.Context) (net.Conn, error) {
		near, far := net.Pipe()
		go func() {
			defer func() { _ = far.Close() }()
			_ = daemon.ServeMachine(context.WithoutCancel(ctx), far, func() (*daemon.Pairing, error) { return pairing, nil }, servePairedTestRPC)
		}()
		return near, nil
	}
}

func servePairedTestRPC(ctx context.Context, stream io.ReadWriteCloser) error {
	peer := rpc.NewConn(ctx, stream, func(_ context.Context, method string, _ jsontext.Value) (jsontext.Value, error) {
		switch method {
		case "initialize":
			return jsontext.Value(`{"userAgent":"dalgona/test","codexHome":"/fixture","platformFamily":"unix","platformOs":"linux"}`), nil
		case "initialized":
			return nil, nil
		}
		return nil, &rpc.RPCError{Code: -32601, Message: "Method not found"}
	})
	defer func() { _ = peer.Close() }()
	select {
	case <-ctx.Done():
	case <-peer.Done():
	}
	return nil
}

func pairControl(t *testing.T, ctx context.Context, dial webui.Dialer, req rpc.MachineRequest) rpc.MachineReply {
	t.Helper()
	conn, err := dial(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = conn.Close() }()
	reply, err := rpc.ExchangeMachine(ctx, conn, req)
	if err != nil || reply.Status != "" {
		t.Fatalf("control %s: %q, %v", req.Action, reply.Status, err)
	}
	return reply
}

func startPairingRelay(t *testing.T, ctx context.Context, dial webui.Dialer, certFile, keyFile string) string {
	t.Helper()
	ctx, cancel := context.WithCancel(ctx)
	started := make(relayOriginWriter, 1)
	done := make(chan error, 1)
	cfg := webui.Config{Listen: "127.0.0.1:0", CertFile: certFile, KeyFile: keyFile, PairingDial: dial}
	go func() { done <- webui.Serve(ctx, cfg, dial, started) }()
	select {
	case origin := <-started:
		t.Cleanup(func() {
			cancel()
			if err := <-done; err != nil {
				t.Error(err)
			}
		})
		return origin
	case err := <-done:
		cancel()
		t.Fatalf("start TLS relay: %v", err)
	case <-ctx.Done():
		cancel()
		t.Fatal(ctx.Err())
	}
	return ""
}

type relayOriginWriter chan string

func (w relayOriginWriter) Write(p []byte) (int, error) {
	if origin, _, found := strings.Cut(string(p), "/#token="); found {
		w <- origin
	}
	return len(p), nil
}

func pairingCertificate(t *testing.T) (certFile, keyFile, pin string) {
	t.Helper()
	private, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	template := x509.Certificate{
		SerialNumber: big.NewInt(1), IPAddresses: []net.IP{net.IPv4(127, 0, 0, 1)},
		NotBefore: time.Now().Add(-time.Hour), NotAfter: time.Now().Add(time.Hour),
		KeyUsage:    x509.KeyUsageCertSign | x509.KeyUsageDigitalSignature,
		ExtKeyUsage: []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth}, BasicConstraintsValid: true, IsCA: true,
	}
	der, err := x509.CreateCertificate(rand.Reader, &template, &template, &private.PublicKey, private)
	if err != nil {
		t.Fatal(err)
	}
	pairCert, err := x509.ParseCertificate(der)
	if err != nil {
		t.Fatal(err)
	}
	keyDER, err := x509.MarshalECPrivateKey(private)
	if err != nil {
		t.Fatal(err)
	}
	home := t.TempDir()
	certFile, keyFile = filepath.Join(home, "cert.pem"), filepath.Join(home, "key.pem")
	writePairingPEM(t, certFile, "CERTIFICATE", der)
	writePairingPEM(t, keyFile, "EC PRIVATE KEY", keyDER)
	digest := sha256.Sum256(pairCert.RawSubjectPublicKeyInfo)
	return certFile, keyFile, hex.EncodeToString(digest[:])
}

func writePairingPEM(t *testing.T, path, kind string, der []byte) {
	t.Helper()
	if err := os.WriteFile(path, pem.EncodeToMemory(&pem.Block{Type: kind, Bytes: der}), 0o600); err != nil {
		t.Fatal(err)
	}
}
