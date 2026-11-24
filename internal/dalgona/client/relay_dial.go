package client

import (
	"bytes"
	"context"
	"crypto/tls"
	"crypto/x509"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"

	"github.com/coder/websocket"

	"github.com/gosuda/dalgona/internal/dal/config"
)

const (
	relayPath      = "rpc"
	relayReadLimit = 16 << 20
)

var (
	errRelayToken  = errors.New("the relay refused the token in token_file; copy the [web] token of the remote into it")
	errRelayBinary = errors.New("the relay sent a binary message")
)

func (r *Remotes) Relay(ctx context.Context, name string) (io.ReadWriteCloser, error) {
	r.mu.Lock()
	spec, ok := r.remotes[name]
	r.mu.Unlock()
	if !ok || spec.Kind != config.RemoteDaemon {
		return nil, fmt.Errorf("remote: no [remotes.%s] entry with kind = \"daemon\"", name)
	}
	target, err := config.ParseDaemonURL(spec.URL)
	if err != nil {
		return nil, refusal{fmt.Errorf("remote %s: url %w", name, err)}
	}
	token, err := config.ReadTokenFile(spec.TokenFile)
	if err != nil {
		return nil, refusal{fmt.Errorf("remote %s: %w", name, err)}
	}
	roots, err := config.TLSRoots(r.tlsCAFile)
	if err != nil {
		return nil, refusal{fmt.Errorf("remote %s: %w", name, err)}
	}
	conn, err := dialRelay(ctx, target.JoinPath(relayPath).String(), token, roots)
	if err != nil {
		return nil, fmt.Errorf("remote %s: %w", name, err)
	}
	return conn, nil
}

func dialRelay(ctx context.Context, target, token string, roots *x509.CertPool) (io.ReadWriteCloser, error) {
	transport := &http.Transport{
		Proxy:           http.ProxyFromEnvironment,
		TLSClientConfig: &tls.Config{MinVersion: tls.VersionTLS13, RootCAs: roots},
	}
	defer transport.CloseIdleConnections()
	client := &http.Client{
		Transport:     transport,
		CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse },
	}
	options := &websocket.DialOptions{HTTPClient: client, HTTPHeader: http.Header{"Authorization": {"Bearer " + token}}}
	ws, resp, err := websocket.Dial(ctx, target, options)
	if err != nil {
		return nil, relayDialError(resp, err)
	}
	return relayStream(ws), nil
}

func relayStream(ws *websocket.Conn) *relayConn {
	ws.SetReadLimit(relayReadLimit)
	ctx, cancel := context.WithCancel(context.Background())
	return &relayConn{ws: ws, ctx: ctx, cancel: cancel}
}

func relayDialError(resp *http.Response, err error) error {
	if resp != nil && resp.StatusCode == http.StatusUnauthorized {
		return refusal{errRelayToken}
	}
	if _, untrusted := errors.AsType[*tls.CertificateVerificationError](err); untrusted {
		return refusal{err}
	}
	return err
}

type relayConn struct {
	ws      *websocket.Conn
	ctx     context.Context
	cancel  context.CancelFunc
	message io.Reader
	newline bool
	partial []byte
}

func (c *relayConn) Read(p []byte) (int, error) {
	if len(p) == 0 {
		return 0, nil
	}
	if c.newline {
		c.newline = false
		p[0] = '\n'
		return 1, nil
	}
	if c.message == nil {
		if err := c.nextMessage(); err != nil {
			return 0, err
		}
	}
	n, err := c.message.Read(p)
	if !errors.Is(err, io.EOF) {
		return n, c.readError(err)
	}
	c.message = nil
	if n == len(p) {
		c.newline = true
		return n, nil
	}
	p[n] = '\n'
	return n + 1, nil
}

func (c *relayConn) nextMessage() error {
	kind, message, err := c.ws.Reader(c.ctx)
	switch {
	case err != nil:
		return c.readError(err)
	case kind != websocket.MessageText:
		return errRelayBinary
	}
	c.message = message
	return nil
}

func (c *relayConn) readError(err error) error {
	switch {
	case err == nil:
		return nil
	case c.ctx.Err() != nil:
		return net.ErrClosed
	case websocket.CloseStatus(err) != -1:
		return io.EOF
	}
	return err
}

func (c *relayConn) Write(p []byte) (int, error) {
	rest := p
	for {
		line, after, found := bytes.Cut(rest, []byte{'\n'})
		if !found {
			c.partial = append(c.partial, rest...)
			return len(p), nil
		}
		if len(c.partial) > 0 {
			c.partial = append(c.partial, line...)
			line = c.partial
		}
		if err := c.ws.Write(c.ctx, websocket.MessageText, line); err != nil {
			return len(p) - len(rest), err
		}
		c.partial = c.partial[:0]
		rest = after
	}
}

func (c *relayConn) Close() error {
	c.cancel()
	return c.ws.CloseNow()
}
