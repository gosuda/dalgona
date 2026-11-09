package webui

import (
	"bufio"
	"bytes"
	"context"
	"errors"
	"io"
	"net"
	"net/http"
	"time"

	"github.com/coder/websocket"
)

const (
	maxMessageBytes = 16<<20 - 1
	lineBufferBytes = 64 << 10
	dialTimeout     = 10 * time.Second
)

var (
	errBinaryMessage  = errors.New("rpc message is not text")
	errMessageNewline = errors.New("rpc message contains a newline")
)

func (s *server) rpc(w http.ResponseWriter, r *http.Request) {
	if !s.authorized(r.URL.Query().Get("token")) {
		http.Error(w, http.StatusText(http.StatusUnauthorized), http.StatusUnauthorized)
		return
	}
	s.relays.Add(1)
	defer s.relays.Done()
	daemon, err := s.dialDaemon(r.Context())
	if err != nil {
		s.logf("web: dial daemon: %v\n", err)
		http.Error(w, "daemon unavailable", http.StatusBadGateway)
		return
	}
	browser, err := websocket.Accept(w, r, nil)
	if err != nil {
		_ = daemon.Close()
		return
	}
	browser.SetReadLimit(maxMessageBytes)
	s.relay(r.Context(), browser, daemon)
}

func (s *server) dialDaemon(ctx context.Context) (net.Conn, error) {
	ctx, cancel := context.WithTimeout(ctx, dialTimeout)
	defer cancel()
	return s.dial(ctx)
}

func (s *server) relay(ctx context.Context, browser *websocket.Conn, daemon net.Conn) {
	done := make(chan error, 2)
	go func() { done <- toDaemon(ctx, browser, daemon) }()
	go func() { done <- toBrowser(ctx, browser, daemon) }()
	first := <-done
	_ = daemon.Close()
	code, reason := closeStatus(first)
	_ = browser.Close(code, reason)
	second := <-done
	if code == websocket.StatusInternalError && ctx.Err() == nil {
		s.logf("web: rpc relay: %v\n", errors.Join(first, second))
	}
}

func closeStatus(err error) (websocket.StatusCode, string) {
	switch {
	case websocket.CloseStatus(err) != -1:
		return websocket.StatusNormalClosure, ""
	case errors.Is(err, errBinaryMessage):
		return websocket.StatusUnsupportedData, err.Error()
	case errors.Is(err, errMessageNewline):
		return websocket.StatusInvalidFramePayloadData, err.Error()
	case errors.Is(err, io.EOF):
		return websocket.StatusGoingAway, "daemon closed the connection"
	default:
		return websocket.StatusInternalError, "rpc relay failed"
	}
}

func toDaemon(ctx context.Context, browser *websocket.Conn, daemon io.Writer) error {
	var line bytes.Buffer
	for {
		kind, message, err := browser.Reader(ctx)
		if err != nil {
			return err
		}
		if kind != websocket.MessageText {
			return errBinaryMessage
		}
		line.Reset()
		if _, err := line.ReadFrom(message); err != nil {
			return err
		}
		if bytes.IndexByte(line.Bytes(), '\n') >= 0 {
			return errMessageNewline
		}
		line.WriteByte('\n')
		if _, err := daemon.Write(line.Bytes()); err != nil {
			return err
		}
	}
}

func toBrowser(ctx context.Context, browser *websocket.Conn, daemon io.Reader) error {
	lines := bufio.NewReaderSize(daemon, lineBufferBytes)
	for {
		chunk, err := lines.ReadSlice('\n')
		switch {
		case err == nil:
			err = browser.Write(ctx, websocket.MessageText, chunk[:len(chunk)-1])
		case errors.Is(err, bufio.ErrBufferFull):
			err = streamLine(ctx, browser, lines, chunk)
		}
		if err != nil {
			return err
		}
	}
}

func streamLine(ctx context.Context, browser *websocket.Conn, lines *bufio.Reader, chunk []byte) error {
	message, err := browser.Writer(ctx, websocket.MessageText)
	if err != nil {
		return err
	}
	readErr := bufio.ErrBufferFull
	for errors.Is(readErr, bufio.ErrBufferFull) {
		if _, err := message.Write(chunk); err != nil {
			return err
		}
		chunk, readErr = lines.ReadSlice('\n')
	}
	if readErr != nil {
		return readErr
	}
	if _, err := message.Write(chunk[:len(chunk)-1]); err != nil {
		return err
	}
	return message.Close()
}
