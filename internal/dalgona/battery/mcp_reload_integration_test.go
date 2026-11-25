package battery_test

import (
	"bytes"
	"context"
	"io"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/app"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dalgona/battery"
)

func TestOpenThreadOffersReplacementMCPToolsOnNextTurn(t *testing.T) {
	server := buildMCPServer(t)
	home, _ := trustEnv(t, "")
	if err := config.WriteSettings(home, config.SettingEdit{
		Key: "mcp_servers.fix", Value: map[string]any{"command": server},
	}); err != nil {
		t.Fatal(err)
	}
	requests := recordModelRequests(t)
	d := &battery.Daemon{}
	cwd := t.TempDir()
	cfg, err := config.Load(config.ProductDalgona, cwd, config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	cfg.ToolMode = config.ToolModeNormal
	host, closeHost, err := app.NewHost(t.Context(), app.Options{Product: config.ProductDalgona, Config: &cfg, Batteries: d.Load})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = closeHost() })
	thread, err := host.Start(t.Context(), agent.ThreadSpec{Cwd: cwd, Model: "openai/gpt-test"})
	if err != nil {
		t.Fatal(err)
	}
	if first := submitAndReadModelRequest(t, thread, requests); !bytes.Contains(first, []byte(`"name":"fix__echo"`)) {
		t.Fatal("the original thread turn did not offer the original MCP tool")
	}
	replaceConfiguredMCP(t, d, home, server)
	second := submitAndReadModelRequest(t, thread, requests)
	if bytes.Contains(second, []byte(`"name":"fix__echo"`)) || !bytes.Contains(second, []byte(`"name":"replacement__echo"`)) {
		t.Fatal("the same thread and model did not replace the old tool with the new tool")
	}
}

func recordModelRequests(t *testing.T) <-chan []byte {
	t.Helper()
	requests := make(chan []byte, 2)
	api := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method == http.MethodGet {
			w.Header().Set("Content-Type", "application/json")
			_, _ = io.WriteString(w, `{"data":[{"id":"gpt-test"}]}`)
			return
		}
		body, err := io.ReadAll(r.Body)
		if err != nil {
			t.Error(err)
			w.WriteHeader(http.StatusBadRequest)
			return
		}
		if bytes.Contains(body, []byte(`"name":"fix__echo"`)) || bytes.Contains(body, []byte(`"name":"replacement__echo"`)) {
			select {
			case requests <- body:
			default:
				t.Error("more MCP-enabled model requests than completed turns")
			}
		}
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = io.WriteString(w, "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"ok\"}\n\n"+
			"event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n")
	}))
	t.Cleanup(api.Close)
	t.Setenv("OPENAI_BASE_URL", api.URL)
	t.Setenv("OPENAI_API_KEY", "fixture")
	return requests
}

func replaceConfiguredMCP(t *testing.T, d *battery.Daemon, home, server string) {
	t.Helper()
	if err := config.WriteSettings(home,
		config.SettingEdit{Key: "mcp_servers.fix"},
		config.SettingEdit{Key: "mcp_servers.replacement", Value: map[string]any{"command": server}},
	); err != nil {
		t.Fatal(err)
	}
	if _, err := d.ReloadUserMCP(); err != nil {
		t.Fatal(err)
	}
}

func submitAndReadModelRequest(t *testing.T, thread *agent.Thread, requests <-chan []byte) []byte {
	t.Helper()
	ctx, cancel := context.WithTimeout(t.Context(), 30*time.Second)
	defer cancel()
	updates := thread.Subscribe()
	defer thread.Unsubscribe(updates)
	if err := thread.Submit(ctx, agent.Input{Text: "list tools"}); err != nil {
		t.Fatal(err)
	}
	for {
		select {
		case update := <-updates:
			if _, complete := update.Event.(event.TurnCompleted); !complete {
				continue
			}
			select {
			case request := <-requests:
				return request
			default:
				t.Fatal("turn completed without a request to the model fixture")
			}
		case <-ctx.Done():
			t.Fatal("turn did not complete", ctx.Err())
		}
	}
}
