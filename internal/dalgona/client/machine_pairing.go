package client

import (
	"bytes"
	"context"
	"crypto/rand"
	"crypto/sha256"
	"crypto/tls"
	"encoding/base64"
	"encoding/hex"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"path/filepath"
	"sync/atomic"
	"time"

	"github.com/coder/websocket"

	"github.com/gosuda/dalgona/internal/dal/auth"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func (r *Remotes) dialDevice(ctx context.Context, spec config.RemoteSpec) (io.ReadWriteCloser, error) {
	peer, err := r.pairingPeer(rpc.PairingCard{Origin: spec.URL, MachineID: spec.MachineID, PublicKeyPin: spec.PublicKeyPin})
	if err != nil {
		return nil, err
	}
	defer peer.transport.CloseIdleConnections()
	if err = peer.verifyDeviceMachine(ctx); err != nil {
		return nil, err
	}
	credential, err := readDeviceCredential(spec.TokenFile)
	if err != nil {
		return nil, refusal{err}
	}
	return peer.dialDevice(ctx, credential)
}

func readDeviceCredential(path string) (string, error) {
	if err := auth.CheckHomeDir(path); err != nil {
		return "", fmt.Errorf("check device credential access: %w", err)
	}
	data, err := config.ReadTokenFile(path)
	if err != nil {
		return "", err
	}
	var token string
	if err := json.Unmarshal([]byte(data), &token); err != nil || !validDeviceCredential(token) {
		return "", errors.New("device token_file does not contain a valid paired credential; pair again")
	}
	return token, nil
}

func (p *pairingPeer) verifyDeviceMachine(ctx context.Context) error {
	opts := &websocket.DialOptions{HTTPClient: p.http}
	ws, res, err := websocket.Dial(ctx, p.origin.JoinPath(rpc.MachineDevicePath).String(), opts)
	switch {
	case err == nil:
		_ = ws.CloseNow()
		return refusal{errors.New("paired relay upgraded an unauthorized request")}
	case res == nil:
		return relayDialError(nil, err)
	case res.StatusCode != http.StatusUnauthorized:
		return refusal{fmt.Errorf("paired relay identity challenge failed (HTTP %d)", res.StatusCode)}
	}
	return p.checkIdentity(res.Header.Get(rpc.MachineIDHeader))
}

func (p *pairingPeer) dialDevice(ctx context.Context, credential string) (io.ReadWriteCloser, error) {
	opts := &websocket.DialOptions{HTTPClient: p.http, HTTPHeader: http.Header{
		"Authorization": {"Bearer " + credential},
	}}
	opts.HTTPHeader.Set(rpc.MachineIDHeader, p.machineID)
	ws, res, err := websocket.Dial(ctx, p.origin.JoinPath(rpc.MachineDevicePath).String(), opts)
	switch {
	case err == nil:
		return relayStream(ws), nil
	case res == nil:
		return nil, relayDialError(nil, err)
	case res.StatusCode == http.StatusConflict:
		return nil, p.identityConflict(res)
	default:
		return nil, pairingStatusError(res)
	}
}

func (p *pairingPeer) identityConflict(res *http.Response) error {
	if err := p.checkIdentity(res.Header.Get(rpc.MachineIDHeader)); err != nil {
		return err
	}
	return refusal{errors.New("relay refused the expected machine identity")}
}

func (r *Remotes) PairDevice(ctx context.Context, name string, card rpc.PairingCard, code, deviceLabel string) (config.RemoteSpec, error) {
	ctx, cancel := context.WithTimeout(ctx, attemptDeadline)
	defer cancel()
	return r.addMachine(ctx, name, func() (config.RemoteSpec, error) { return r.pair(ctx, card, code, deviceLabel) })
}

func (r *Remotes) pair(ctx context.Context, card rpc.PairingCard, code, label string) (config.RemoteSpec, error) {
	peer, err := r.pairingPeer(card)
	if err != nil {
		return config.RemoteSpec{}, err
	}
	defer peer.transport.CloseIdleConnections()
	if err = peer.verifyMachine(ctx); err != nil {
		return config.RemoteSpec{}, err
	}
	reply, err := peer.exchange(ctx, code, label)
	if err != nil {
		return config.RemoteSpec{}, err
	}
	path := filepath.Join(r.home, "devices", rand.Text()+".json")
	if err := auth.WriteSecure(path, func(e *jsontext.Encoder) error { return e.WriteToken(jsontext.String(reply.Credential)) }); err != nil {
		return config.RemoteSpec{}, fmt.Errorf("device paired but its credential could not be stored; revoke it on the daemon and issue a new grant: %w", err)
	}
	return config.RemoteSpec{Kind: config.RemoteDevice, URL: peer.origin.String(), MachineID: card.MachineID, PublicKeyPin: card.PublicKeyPin, TokenFile: path}, nil
}

type pairingPeer struct {
	origin    *url.URL
	machineID string
	http      *http.Client
	transport *http.Transport
}

func (r *Remotes) pairingPeer(card rpc.PairingCard) (*pairingPeer, error) {
	origin, err := config.ParsePairingOrigin(card.Origin)
	if err != nil {
		return nil, refusal{err}
	}
	if !config.ValidMachineDigest(card.MachineID) || !config.ValidMachineDigest(card.PublicKeyPin) {
		return nil, refusal{errors.New("pairing card needs a machine identity and a SHA-256 public-key pin")}
	}
	roots, err := config.TLSRoots(r.tlsCAFile)
	if err != nil {
		return nil, refusal{err}
	}
	tlsDialer := &tls.Dialer{Config: &tls.Config{
		MinVersion: tls.VersionTLS13, RootCAs: roots, ServerName: origin.Hostname(),
		NextProtos: []string{"http/1.1"}, VerifyConnection: verifyPairingPin(card.PublicKeyPin),
	}}
	var dialed atomic.Bool
	transport := &http.Transport{
		DialTLSContext: func(ctx context.Context, network, addr string) (net.Conn, error) {
			if !dialed.CompareAndSwap(false, true) {
				return nil, errors.New("verified pairing connection closed; no credential was retried")
			}
			return tlsDialer.DialContext(ctx, network, addr)
		},
		MaxConnsPerHost: 1, MaxResponseHeaderBytes: 8192, ResponseHeaderTimeout: 10 * time.Second,
	}
	httpClient := &http.Client{Transport: transport, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
	return &pairingPeer{origin: origin, machineID: card.MachineID, http: httpClient, transport: transport}, nil
}

func verifyPairingPin(pin string) func(tls.ConnectionState) error {
	return func(state tls.ConnectionState) error {
		if len(state.PeerCertificates) != 0 {
			digest := sha256.Sum256(state.PeerCertificates[0].RawSubjectPublicKeyInfo)
			if hex.EncodeToString(digest[:]) == pin {
				return nil
			}
		}
		return refusal{errors.New("relay public key does not match the trusted pairing card; verify the key and pair again")}
	}
}

func (p *pairingPeer) verifyMachine(ctx context.Context) error {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, p.origin.JoinPath(rpc.MachineIdentityPath).String(), nil)
	if err != nil {
		return err
	}
	res, err := p.http.Do(req)
	if err != nil {
		return relayDialError(nil, err)
	}
	defer func() { _ = res.Body.Close() }()
	if res.StatusCode != http.StatusNoContent {
		return refusal{fmt.Errorf("pairing identity unavailable (HTTP %d); verify the TLS relay", res.StatusCode)}
	}
	return p.checkIdentity(res.Header.Get(rpc.MachineIDHeader))
}

func (p *pairingPeer) checkIdentity(actual string) error {
	if !config.ValidMachineDigest(actual) {
		return refusal{errors.New("relay did not report a valid machine identity")}
	}
	if actual != p.machineID {
		return refusal{&MachineMismatchError{Expected: p.machineID, Actual: actual}}
	}
	return nil
}

func (p *pairingPeer) exchange(ctx context.Context, code, label string) (rpc.MachineReply, error) {
	var buf bytes.Buffer
	payload := rpc.MachineRequest{Version: rpc.MachineVersion, Action: rpc.MachineExchange, MachineID: p.machineID, Credential: code, Label: label}
	if err := rpc.WriteMachineFrame(&buf, payload); err != nil {
		return rpc.MachineReply{}, err
	}
	if buf.Len() > rpc.MachinePrefaceLimit+2 {
		return rpc.MachineReply{}, errors.New("pairing code or device label is too long")
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, p.origin.JoinPath(rpc.MachineExchangePath).String(), &buf)
	if err != nil {
		return rpc.MachineReply{}, err
	}
	req.GetBody = nil
	req.Header.Set("Content-Type", "application/octet-stream")
	res, err := p.http.Do(req)
	if err != nil {
		return rpc.MachineReply{}, fmt.Errorf("pairing exchange outcome is unknown; list devices on the daemon and issue a new grant: %w", err)
	}
	defer func() { _ = res.Body.Close() }()
	return p.exchangeReply(res)
}

func (p *pairingPeer) exchangeReply(res *http.Response) (rpc.MachineReply, error) {
	if res.StatusCode == http.StatusConflict {
		return rpc.MachineReply{}, p.identityConflict(res)
	}
	if res.StatusCode != http.StatusOK {
		return rpc.MachineReply{}, pairingStatusError(res)
	}
	var reply rpc.MachineReply
	if err := rpc.ReadMachineFrame(res.Body, rpc.MachinePrefaceLimit, &reply); err != nil || reply.Device == nil || !validDeviceCredential(reply.Credential) || reply.Version != rpc.MachineVersion || reply.Status != "" {
		return rpc.MachineReply{}, errors.New("pairing result is invalid; list devices on the daemon and issue a new grant")
	}
	return reply, p.checkIdentity(reply.MachineID)
}

func validDeviceCredential(token string) bool {
	var raw [32]byte
	if len(token) != base64.RawURLEncoding.EncodedLen(len(raw)) {
		return false
	}
	n, err := base64.RawURLEncoding.Strict().Decode(raw[:], []byte(token))
	return err == nil && n == len(raw)
}

func pairingStatusError(res *http.Response) error {
	if res.StatusCode == http.StatusUnauthorized {
		return refusal{errors.New("the daemon refused the pairing grant or device credential; verify the device and request a new grant")}
	}
	return fmt.Errorf("pairing request refused (HTTP %d)", res.StatusCode)
}
