package mcp

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"runtime/debug"
	"slices"
	"sync"

	"github.com/gosuda/dalgona/internal/tool"
)

const (
	protocolVersion = "2025-11-25"
	clientName      = "dalgona"
)

func clientVersion() string {
	info, ok := debug.ReadBuildInfo()
	if !ok || info.Main.Version == "" {
		return "(devel)"
	}
	return info.Main.Version
}

type Config struct {
	Name    string
	Command string
	Args    []string
	Env     []string
	URL     string
}

type Client struct {
	cfg       Config
	mu        sync.Mutex
	transport transport
	nextID    uint64
	closed    bool
}

type transport interface {
	request(ctx context.Context, id uint64, method string, params jsontext.Value) (jsontext.Value, error)
	notify(ctx context.Context, method string) error
	Close() error
}

func New(ctx context.Context, cfg Config) (*Client, error) {
	if cfg.Name == "" {
		return nil, errors.New("mcp: server name is required")
	}
	if (cfg.Command == "") == (cfg.URL == "") {
		return nil, errors.New("mcp: exactly one of command or URL is required")
	}
	cfg.Args = slices.Clone(cfg.Args)
	cfg.Env = slices.Clone(cfg.Env)
	client := &Client{cfg: cfg}
	var err error
	if cfg.Command != "" {
		client.transport, err = newStdioTransport(cfg.Command, cfg.Args, cfg.Env)
	} else {
		client.transport = newHTTPTransport(cfg.URL)
	}
	if err != nil {
		return nil, err
	}
	if err := client.initializeLocked(ctx); err != nil {
		_ = client.transport.Close()
		return nil, err
	}
	return client, nil
}

func (c *Client) Tools(ctx context.Context) ([]tool.Tool, error) {
	var tools []tool.Tool
	cursor := ""
	seen := make(map[string]struct{})
	for {
		if _, exists := seen[cursor]; exists {
			return nil, fmt.Errorf("mcp: tools/list repeated cursor %q", cursor)
		}
		seen[cursor] = struct{}{}
		page, next, err := c.listToolsPage(ctx, cursor)
		if err != nil {
			return nil, err
		}
		tools = append(tools, page...)
		if next == "" {
			return tools, nil
		}
		cursor = next
	}
}

func (c *Client) Close() error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return nil
	}
	c.closed = true
	if err := c.transport.Close(); err != nil {
		return fmt.Errorf("mcp: close transport: %w", err)
	}
	return nil
}

func (c *Client) call(ctx context.Context, method string, params jsontext.Value) (jsontext.Value, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return nil, fmt.Errorf("mcp: %s: %w", method, io.ErrClosedPipe)
	}
	return c.callLocked(ctx, method, params)
}

func (c *Client) callLocked(ctx context.Context, method string, params jsontext.Value) (jsontext.Value, error) {
	result, err := c.roundTripLocked(ctx, method, params)
	if err == nil {
		return result, nil
	}
	restart := c.cfg.Command != "" && errors.Is(err, errServerCrashed)
	expired := errors.Is(err, errSessionExpired)
	if !restart && !expired {
		return nil, err
	}
	if restart {
		if err := c.restartStdioLocked(ctx); err != nil {
			return nil, err
		}
	} else if err := c.resetHTTPSessionLocked(ctx); err != nil {
		return nil, err
	}
	return c.roundTripLocked(ctx, method, params)
}

func (c *Client) roundTripLocked(ctx context.Context, method string, params jsontext.Value) (jsontext.Value, error) {
	c.nextID++
	result, err := c.transport.request(ctx, c.nextID, method, params)
	if err != nil {
		return nil, fmt.Errorf("mcp: %s: %w", method, err)
	}
	return result, nil
}

func (c *Client) initializeLocked(ctx context.Context) error {
	params, err := encodeInitializeParams()
	if err != nil {
		return fmt.Errorf("mcp: initialize: %w", err)
	}
	c.nextID++
	result, err := c.transport.request(ctx, c.nextID, "initialize", params)
	if err != nil {
		return fmt.Errorf("mcp: initialize: %w", err)
	}
	fields, err := decodeObject(result)
	if err != nil {
		return fmt.Errorf("mcp: initialize result: %w", err)
	}
	version, err := decodeString(fields["protocolVersion"])
	if err != nil {
		return fmt.Errorf("mcp: initialize protocolVersion: %w", err)
	}
	if version != protocolVersion {
		return fmt.Errorf("mcp: unsupported protocol version %q", version)
	}
	if versioned, ok := c.transport.(interface{ setProtocolVersion(string) }); ok {
		versioned.setProtocolVersion(version)
	}
	if err := c.transport.notify(ctx, "notifications/initialized"); err != nil {
		return fmt.Errorf("mcp: initialized notification: %w", err)
	}
	return nil
}

func (c *Client) restartStdioLocked(ctx context.Context) error {
	_ = c.transport.Close()
	transport, err := newStdioTransport(c.cfg.Command, c.cfg.Args, c.cfg.Env)
	if err != nil {
		return fmt.Errorf("mcp: restart stdio server: %w", err)
	}
	c.transport = transport
	if err := c.initializeLocked(ctx); err != nil {
		return fmt.Errorf("mcp: initialize restarted server: %w", err)
	}
	return nil
}

func (c *Client) resetHTTPSessionLocked(ctx context.Context) error {
	resetter, ok := c.transport.(interface{ resetSession() })
	if !ok {
		return errors.New("mcp: session expired on a non-HTTP transport")
	}
	resetter.resetSession()
	if err := c.initializeLocked(ctx); err != nil {
		return fmt.Errorf("mcp: initialize replacement session: %w", err)
	}
	return nil
}
