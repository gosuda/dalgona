package battery_test

import (
	"path/filepath"
	"testing"

	"github.com/gosuda/dalgona/internal/app"
	"github.com/gosuda/dalgona/internal/app/battery"
	"github.com/gosuda/dalgona/internal/config"
)

func TestMCPStatusReportsFailedServer(t *testing.T) {
	trustEnv(t, "")
	cfg, err := config.Load(config.ProductDalgona, t.TempDir(), config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	missing := filepath.Join(t.TempDir(), "no-such-server")
	cfg.MCPServers = map[string]config.MCPServerSpec{"bogus": {Command: missing}}
	extra, err := (&battery.Daemon{}).Load(t.Context(), app.Base{Config: cfg})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if closeErr := extra.Close(); closeErr != nil {
			t.Error(closeErr)
		}
	})
	project := t.TempDir()
	extra.Tools(t.Context(), project)
	statuses := extra.MCPStatus(t.Context(), project)
	if len(statuses) != 1 || statuses[0].Name != "bogus" || statuses[0].Status != "failed" || statuses[0].Error == "" || len(statuses[0].Tools) != 0 {
		t.Fatalf("statuses = %+v, want one failed server with an error and no tools", statuses)
	}
}
