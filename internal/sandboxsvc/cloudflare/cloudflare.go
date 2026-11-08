package cloudflare

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"
	"time"

	"github.com/coder/websocket"
	"github.com/gosuda/dalgona/internal/sandboxsvc"
)

type Probe func(ctx context.Context, id string) bool

type Service struct {
	baseURL string
	token   string
	client  *http.Client
	probe   Probe
}

func New(baseURL, token string, probe ...Probe) *Service {
	var p Probe
	if len(probe) != 0 {
		p = probe[0]
	}
	return &Service{baseURL: strings.TrimRight(baseURL, "/"), token: token, client: http.DefaultClient, probe: p}
}

func NewWithClient(baseURL, token string, client *http.Client, probe Probe) *Service {
	if client == nil {
		client = http.DefaultClient
	}
	return &Service{baseURL: strings.TrimRight(baseURL, "/"), token: token, client: client, probe: probe}
}

func (s *Service) Create(ctx context.Context, spec sandboxsvc.Spec) (string, error) {
	if err := s.post(ctx, spec.Name); err != nil {
		return "", fmt.Errorf("cloudflare: create %s: %w", spec.Name, err)
	}
	return spec.Name, nil
}

func (s *Service) Attach(ctx context.Context, id string) (string, error) {
	if err := s.post(ctx, id); err != nil {
		return "", fmt.Errorf("cloudflare: attach %s: %w", id, err)
	}
	return id, nil
}

func (s *Service) Destroy(ctx context.Context, id string) error {
	req, err := s.request(ctx, http.MethodDelete, id)
	if err != nil {
		return err
	}
	resp, err := s.client.Do(req)
	if err != nil {
		return fmt.Errorf("cloudflare: delete %s: %w", id, err)
	}
	defer resp.Body.Close()
	if resp.StatusCode < http.StatusOK || resp.StatusCode >= http.StatusMultipleChoices {
		return fmt.Errorf("cloudflare: delete %s: status %d", id, resp.StatusCode)
	}
	return nil
}

func (s *Service) Connect(ctx context.Context, id string, endpoint sandboxsvc.Endpoint) (io.ReadWriteCloser, error) {
	path := "/"
	if endpoint == sandboxsvc.Bridge {
		path = "/bridge"
	}
	u, err := s.websocketURL(id, path)
	if err != nil {
		return nil, err
	}
	header := http.Header{}
	if s.token != "" {
		header.Set("Authorization", "Bearer "+s.token)
	}
	conn, _, err := websocket.Dial(ctx, u, &websocket.DialOptions{HTTPHeader: header})
	if err != nil {
		return nil, fmt.Errorf("cloudflare: connect %s: %w", id, err)
	}
	return websocket.NetConn(ctx, conn, websocket.MessageBinary), nil
}

func (s *Service) Replaced(ctx context.Context, id string) bool {
	if s.probe != nil {
		return s.probe(ctx, id)
	}
	conn, err := s.Connect(ctx, id, sandboxsvc.ExecServer)
	if err != nil {
		return false
	}
	missing, probeErr := probeBaseline(ctx, conn)
	return probeErr == nil && missing
}

func probeBaseline(ctx context.Context, conn io.ReadWriteCloser) (bool, error) {
	defer func() { _ = conn.Close() }()
	stop := context.AfterFunc(ctx, func() { _ = conn.Close() })
	defer stop()
	if deadline, ok := conn.(interface{ SetReadDeadline(time.Time) error }); ok {
		_ = deadline.SetReadDeadline(time.Now().Add(5 * time.Second))
	}
	request := []byte(`{"id":1,"method":"fs/readFile","params":{"path":"/workspace/.dalgona-baseline"}}` + "\n")
	if _, err := conn.Write(request); err != nil {
		return false, fmt.Errorf("cloudflare: write baseline probe: %w", err)
	}
	reader := bufio.NewReaderSize(conn, 4096)
	line, err := reader.ReadBytes('\n')
	if err != nil {
		return false, fmt.Errorf("cloudflare: read baseline probe: %w", err)
	}
	var response struct {
		Error jsontext.Value `json:"error"`
	}
	if decodeErr := json.Unmarshal(line, &response); decodeErr != nil {
		return false, fmt.Errorf("cloudflare: decode baseline probe: %w", decodeErr)
	}
	return baselineErrorMissing(response.Error), nil
}

func baselineErrorMissing(raw jsontext.Value) bool {
	if len(raw) == 0 || bytes.Equal(bytes.TrimSpace(raw), []byte("null")) {
		return false
	}
	var wireError struct {
		Code int `json:"code"`
	}
	if err := json.Unmarshal(raw, &wireError); err != nil {
		return false
	}
	return wireError.Code == -32004
}

func (s *Service) post(ctx context.Context, id string) error {
	req, err := s.request(ctx, http.MethodPost, id)
	if err != nil {
		return err
	}
	resp, err := s.client.Do(req)
	if err != nil {
		return fmt.Errorf("cloudflare: start %s: %w", id, err)
	}
	defer resp.Body.Close()
	if resp.StatusCode < http.StatusOK || resp.StatusCode >= http.StatusMultipleChoices {
		return fmt.Errorf("cloudflare: start %s: status %d", id, resp.StatusCode)
	}
	return nil
}

func (s *Service) request(ctx context.Context, method, id string) (*http.Request, error) {
	if s.baseURL == "" {
		return nil, errors.New("cloudflare: empty worker URL")
	}
	u, err := url.JoinPath(s.baseURL, "box", id)
	if err != nil {
		return nil, fmt.Errorf("cloudflare: build URL: %w", err)
	}
	req, err := http.NewRequestWithContext(ctx, method, u, http.NoBody)
	if err != nil {
		return nil, fmt.Errorf("cloudflare: build request: %w", err)
	}
	if len(s.token) > 0 {
		req.Header.Set("Authorization", "Bearer "+s.token)
	}
	return req, nil
}

func (s *Service) websocketURL(id, suffix string) (string, error) {
	var u string
	var err error
	if suffix == "/" {
		u, err = url.JoinPath(s.baseURL, "box", id)
	} else {
		u, err = url.JoinPath(s.baseURL, "box", id, suffix)
	}
	if err != nil {
		return "", fmt.Errorf("cloudflare: build websocket URL: %w", err)
	}
	parsed, err := url.Parse(u)
	if err != nil {
		return "", fmt.Errorf("cloudflare: parse websocket URL: %w", err)
	}
	switch parsed.Scheme {
	case "http":
		parsed.Scheme = "ws"
	case "https":
		parsed.Scheme = "wss"
	case "ws", "wss":
	default:
		return "", fmt.Errorf("cloudflare: unsupported URL scheme %q", parsed.Scheme)
	}
	return parsed.String(), nil
}
