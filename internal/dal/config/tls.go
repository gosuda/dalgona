package config

import (
	"crypto/tls"
	"crypto/x509"
	"fmt"
	"net/http"
	"os"
)

func TLSRoots(path string) (*x509.CertPool, error) {
	if path == "" {
		return nil, nil
	}
	roots, err := x509.SystemCertPool()
	if err != nil {
		return nil, fmt.Errorf("tls_ca_file: system cert pool: %w", err)
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("tls_ca_file: %w", err)
	}
	if !roots.AppendCertsFromPEM(data) {
		return nil, fmt.Errorf("tls_ca_file %s: holds no PEM certificate", path)
	}
	return roots, nil
}

func InstallTLSRoots(path string) error {
	if path == "" {
		return nil
	}
	roots, err := TLSRoots(path)
	if err != nil {
		return err
	}
	base, ok := http.DefaultTransport.(*http.Transport)
	if !ok {
		return fmt.Errorf("tls_ca_file: http.DefaultTransport is %T", http.DefaultTransport)
	}
	transport := base.Clone()
	if transport.TLSClientConfig == nil {
		transport.TLSClientConfig = &tls.Config{MinVersion: tls.VersionTLS12}
	}
	transport.TLSClientConfig.RootCAs = roots
	http.DefaultTransport = transport
	return nil
}
