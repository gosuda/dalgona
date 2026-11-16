package config_test

import (
	"bytes"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/pem"
	"math/big"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/config"
)

func writeCACertificate(t *testing.T, path string) {
	t.Helper()
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	template := x509.Certificate{
		SerialNumber:          big.NewInt(1),
		Subject:               pkix.Name{CommonName: "test-ca"},
		NotBefore:             time.Unix(0, 0),
		NotAfter:              time.Now().Add(time.Hour),
		KeyUsage:              x509.KeyUsageCertSign,
		BasicConstraintsValid: true,
		IsCA:                  true,
	}
	der, err := x509.CreateCertificate(rand.Reader, &template, &template, &key.PublicKey, key)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: der}), 0o600); err != nil {
		t.Fatal(err)
	}
}

func loadTLSConfig(t *testing.T, home, body string) (config.Config, error) {
	t.Helper()
	t.Setenv("DALGONA_HOME", home)
	writeConfigFile(t, home, "config.toml", body)
	return config.Load(config.ProductDalgona, t.TempDir(), config.Flags{})
}

func TestConfigTLSCAFileErrors(t *testing.T) {
	home := t.TempDir()
	writeCACertificate(t, filepath.Join(home, "ca.pem"))
	if err := os.Mkdir(filepath.Join(home, "bundle"), 0o755); err != nil {
		t.Fatal(err)
	}
	large := filepath.Join(home, "large.pem")
	if err := os.WriteFile(large, make([]byte, 1048577), 0o600); err != nil {
		t.Fatal(err)
	}
	plain := filepath.Join(home, "plain.pem")
	if err := os.WriteFile(plain, []byte("not a certificate\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	tests := []struct {
		name string
		body string
		want string
	}{
		{
			"missing file",
			"tls_ca_file = \"missing.pem\"\n",
			"config.toml:1: tls_ca_file " + filepath.Join(home, "missing.pem") + ": no such file",
		},
		{
			"directory",
			"tls_ca_file = \"bundle\"\n",
			"config.toml:1: tls_ca_file " + filepath.Join(home, "bundle") + ": not a regular file",
		},
		{
			"over the size limit",
			"tls_ca_file = \"large.pem\"\n",
			"config.toml:1: tls_ca_file " + large + ": larger than 1048576 bytes",
		},
		{
			"no PEM certificate",
			"tls_ca_file = \"plain.pem\"\n",
			"config.toml:1: tls_ca_file " + plain + ": holds no PEM certificate",
		},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			_, err := loadTLSConfig(t, home, test.body)
			requireConfigError(t, err, test.want)
		})
	}
}

func requireTLSCAFile(t *testing.T, home, body, want string) {
	t.Helper()
	cfg, err := loadTLSConfig(t, home, body)
	if err != nil {
		t.Fatal(err)
	}
	if cfg.TLSCAFile != want {
		t.Fatalf("tls_ca_file = %q, want %q", cfg.TLSCAFile, want)
	}
}

func writePaddedCACertificate(t *testing.T, home string) {
	t.Helper()
	caBytes, err := os.ReadFile(filepath.Join(home, "ca.pem"))
	if err != nil {
		t.Fatal(err)
	}
	exact := append(bytes.Clone(caBytes), bytes.Repeat([]byte{'\n'}, 1048576-len(caBytes))...)
	if len(exact) != 1048576 {
		t.Fatalf("padded file holds %d bytes, want 1048576", len(exact))
	}
	if err = os.WriteFile(filepath.Join(home, "exact.pem"), exact, 0o600); err != nil {
		t.Fatal(err)
	}
}

func TestConfigTLSCAFileResolvesAndAccepts(t *testing.T) {
	home := t.TempDir()
	writeCACertificate(t, filepath.Join(home, "ca.pem"))
	requireTLSCAFile(t, home, "tls_ca_file = \"ca.pem\"\n", filepath.Join(home, "ca.pem"))
	requireTLSCAFile(t, home, "tls_ca_file = \""+filepath.Join(home, "ca.pem")+"\"\n", filepath.Join(home, "ca.pem"))
	writePaddedCACertificate(t, home)
	requireTLSCAFile(t, home, "tls_ca_file = \"exact.pem\"\n", filepath.Join(home, "exact.pem"))
	requireTLSCAFile(t, home, "model = \"openai/gpt-test\"\n", "")
}
