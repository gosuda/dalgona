package app

import (
	"crypto/tls"
	"crypto/x509"
	"fmt"
	"net/http"
	"os"

	"github.com/gosuda/dalgona/internal/dal/config"
)

func InstallTLSRoots(cfg config.Config) error {
	if cfg.TLSCAFile == "" {
		return nil
	}
	roots, err := x509.SystemCertPool()
	if err != nil {
		return fmt.Errorf("tls_ca_file: system cert pool: %w", err)
	}
	data, err := os.ReadFile(cfg.TLSCAFile)
	if err != nil {
		return fmt.Errorf("tls_ca_file: %w", err)
	}
	if !roots.AppendCertsFromPEM(data) {
		return fmt.Errorf("tls_ca_file %s: holds no PEM certificate", cfg.TLSCAFile)
	}
	base, ok := http.DefaultTransport.(*http.Transport)
	if !ok {
		return fmt.Errorf("tls_ca_file: http.DefaultTransport is %T", http.DefaultTransport)
	}
	transport := base.Clone()
	if transport.TLSClientConfig == nil {
		transport.TLSClientConfig = &tls.Config{}
	}
	transport.TLSClientConfig.RootCAs = roots
	http.DefaultTransport = transport
	return nil
}
