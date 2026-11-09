package webui

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"crypto/subtle"
	"crypto/tls"
	"embed"
	"encoding/base64"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"net"
	"net/http"
	"sync"
	"time"
	"unicode/utf8"
)

//go:embed all:dist
var embedded embed.FS

const (
	minLANTokenChars  = 32
	randomTokenBytes  = 32
	shutdownTimeout   = 5 * time.Second
	readHeaderTimeout = 10 * time.Second
	readTimeout       = 30 * time.Second
	writeTimeout      = 60 * time.Second
	idleTimeout       = 120 * time.Second
	notBuiltMessage   = "web UI not built; run just web"
)

var errPartialTLS = errors.New("web: cert_file and key_file must both be set")

type Config struct {
	Listen   string
	Token    string
	CertFile string
	KeyFile  string
}

type Dialer func(ctx context.Context) (net.Conn, error)

type server struct {
	tokenDigest [sha256.Size]byte
	dial        Dialer
	logMu       sync.Mutex
	relays      sync.WaitGroup
	log         io.Writer
}

func Serve(ctx context.Context, cfg Config, dial Dialer, log io.Writer) error {
	site, err := fs.Sub(embedded, "dist")
	if err != nil {
		return fmt.Errorf("web: open embedded dist: %w", err)
	}
	return serve(ctx, cfg, site, dial, log)
}

func serve(ctx context.Context, cfg Config, site fs.FS, dial Dialer, log io.Writer) error {
	lan := !isLoopback(cfg.Listen)
	token, err := resolveToken(cfg, lan)
	if err != nil {
		return err
	}
	tlsConfig, err := loadTLS(cfg)
	if err != nil {
		return err
	}
	pages, err := pageHandler(site)
	if err != nil {
		return err
	}
	ln, err := listen(ctx, cfg.Listen, tlsConfig)
	if err != nil {
		return err
	}
	s := &server{tokenDigest: sha256.Sum256([]byte(token)), dial: dial, log: log}
	addr := ln.Addr().String()
	scheme := "https"
	if tlsConfig == nil {
		scheme = "http"
		if lan {
			s.logf("web: the token travels unencrypted on %s; set cert_file and key_file to use TLS\n", addr)
		}
	}
	s.logf("%s://%s/#token=%s\n", scheme, addr, token)
	return s.run(ctx, ln, pages)
}

func isLoopback(listen string) bool {
	host, _, err := net.SplitHostPort(listen)
	if err != nil {
		return false
	}
	if host == "localhost" {
		return true
	}
	ip := net.ParseIP(host)
	return ip != nil && ip.IsLoopback()
}

func resolveToken(cfg Config, lan bool) (string, error) {
	if lan && utf8.RuneCountInString(cfg.Token) < minLANTokenChars {
		return "", fmt.Errorf("web: listening on %s needs [web] token (32+ characters)", cfg.Listen)
	}
	if cfg.Token != "" {
		if !tokenIsFragmentSafe(cfg.Token) {
			return "", fmt.Errorf("web: [web] token must contain only [A-Za-z0-9_-]; '+' and '&' cannot survive the URL fragment")
		}
		return cfg.Token, nil
	}
	raw := make([]byte, randomTokenBytes)
	if _, err := rand.Read(raw); err != nil {
		return "", fmt.Errorf("web: generate token: %w", err)
	}
	return base64.RawURLEncoding.EncodeToString(raw), nil
}

func tokenIsFragmentSafe(token string) bool {
	for _, r := range token {
		switch {
		case r >= 'a' && r <= 'z', r >= 'A' && r <= 'Z', r >= '0' && r <= '9', r == '-', r == '_':
		default:
			return false
		}
	}
	return true
}

func loadTLS(cfg Config) (*tls.Config, error) {
	if cfg.CertFile == "" && cfg.KeyFile == "" {
		return nil, nil
	}
	if cfg.CertFile == "" || cfg.KeyFile == "" {
		return nil, errPartialTLS
	}
	cert, err := tls.LoadX509KeyPair(cfg.CertFile, cfg.KeyFile)
	if err != nil {
		return nil, fmt.Errorf("web: load cert_file and key_file: %w", err)
	}
	return &tls.Config{MinVersion: tls.VersionTLS13, Certificates: []tls.Certificate{cert}}, nil
}

func pageHandler(site fs.FS) (http.Handler, error) {
	entries, err := fs.ReadDir(site, ".")
	if err != nil {
		return nil, fmt.Errorf("web: read dist: %w", err)
	}
	for _, entry := range entries {
		if entry.Name() != ".gitkeep" {
			return http.FileServerFS(site), nil
		}
	}
	return http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		http.Error(w, notBuiltMessage, http.StatusServiceUnavailable)
	}), nil
}

func listen(ctx context.Context, addr string, tlsConfig *tls.Config) (net.Listener, error) {
	var lc net.ListenConfig
	ln, err := lc.Listen(ctx, "tcp", addr)
	if err != nil {
		return nil, fmt.Errorf("web: listen on %s: %w", addr, err)
	}
	if tlsConfig != nil {
		return tls.NewListener(ln, tlsConfig), nil
	}
	return ln, nil
}

func (s *server) logf(format string, args ...any) {
	s.logMu.Lock()
	defer s.logMu.Unlock()
	fmt.Fprintf(s.log, format, args...)
}

func (s *server) run(ctx context.Context, ln net.Listener, pages http.Handler) error {
	mux := http.NewServeMux()
	mux.HandleFunc("/rpc", s.rpc)
	mux.Handle("/", pages)
	relayCtx, stopRelays := context.WithCancel(ctx)
	defer stopRelays()
	srv := &http.Server{
		Handler:           mux,
		ReadHeaderTimeout: readHeaderTimeout,
		ReadTimeout:       readTimeout,
		WriteTimeout:      writeTimeout,
		IdleTimeout:       idleTimeout,
		BaseContext:       func(net.Listener) context.Context { return relayCtx },
	}
	served := make(chan error, 1)
	go func() { served <- srv.Serve(ln) }()
	var serveErr error
	select {
	case serveErr = <-served:
	case <-ctx.Done():
	}
	stopRelays()
	shutdownErr := shutdown(ctx, srv)
	if serveErr == nil {
		serveErr = <-served
	}
	s.relays.Wait()
	if errors.Is(serveErr, http.ErrServerClosed) {
		serveErr = nil
	}
	if err := errors.Join(serveErr, shutdownErr); err != nil {
		return fmt.Errorf("web: serve: %w", err)
	}
	return nil
}

func shutdown(ctx context.Context, srv *http.Server) error {
	shutdownCtx, cancel := context.WithTimeout(context.WithoutCancel(ctx), shutdownTimeout)
	defer cancel()
	return srv.Shutdown(shutdownCtx)
}

func (s *server) authorized(token string) bool {
	digest := sha256.Sum256([]byte(token))
	return subtle.ConstantTimeCompare(digest[:], s.tokenDigest[:]) == 1
}
