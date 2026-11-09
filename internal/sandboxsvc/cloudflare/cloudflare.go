package cloudflare

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"

	"github.com/coder/websocket"
	"github.com/gosuda/dalgona/internal/sandboxsvc"
)

type Service struct {
	baseURL string
	token   string
	client  *http.Client
}

func New(baseURL, token string) *Service {
	return &Service{baseURL: strings.TrimRight(baseURL, "/"), token: token, client: http.DefaultClient}
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
	if resp.StatusCode == http.StatusNotFound {
		return fmt.Errorf("cloudflare: delete %s: status 404: %w", id, sandboxsvc.ErrGone)
	}
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
	return websocket.NetConn(ctx, conn, websocket.MessageText), nil
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
