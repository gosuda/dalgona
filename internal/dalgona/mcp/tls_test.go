package mcp_test

import (
	"encoding/pem"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/app"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dalgona/mcp"
)

func TestPrivateCATrustsMCPTransport(t *testing.T) {
	server := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, err := io.ReadAll(r.Body)
		if err != nil {
			http.Error(w, err.Error(), http.StatusBadRequest)
			return
		}
		if strings.Contains(string(body), `"method":"initialize"`) {
			w.Header().Set("Content-Type", "application/json")
			_, _ = io.WriteString(w, `{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-11-25"}}`)
			return
		}
		w.WriteHeader(http.StatusAccepted)
	}))
	t.Cleanup(server.Close)
	caFile := filepath.Join(t.TempDir(), "ca.pem")
	certificate := pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: server.Certificate().Raw})
	if err := os.WriteFile(caFile, certificate, 0o600); err != nil {
		t.Fatal(err)
	}
	dial := func() error {
		client, err := mcp.New(t.Context(), mcp.Config{Name: "private", URL: server.URL})
		if err != nil {
			return err
		}
		return client.Close()
	}
	if err := dial(); err == nil || !strings.Contains(err.Error(), "x509: certificate signed by unknown authority") {
		t.Fatalf("untrusted transport error = %v, want unknown authority", err)
	}
	saved := http.DefaultTransport
	t.Cleanup(func() { http.DefaultTransport = saved })
	if err := app.InstallTLSRoots(config.Config{TLSCAFile: caFile}); err != nil {
		t.Fatal(err)
	}
	if err := dial(); err != nil {
		t.Fatalf("trusted transport: %v", err)
	}
}
