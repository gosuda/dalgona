package report

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestConsentRoundTrip(t *testing.T) {
	home := t.TempDir()
	if ConsentStored(home) {
		t.Fatal("consent stored before write")
	}
	if err := WriteConsent(home, "gh", time.Now()); err != nil {
		t.Fatal(err)
	}
	if !ConsentStored(home) {
		t.Fatal("consent not stored")
	}
	if !ConsentValid(home, "gh") {
		t.Fatal("consent invalid for written method")
	}
	if ConsentValid(home, "app") {
		t.Fatal("consent valid for a different method")
	}
}

func TestConsentInvalidText(t *testing.T) {
	home := t.TempDir()
	if err := WriteConsent(home, "app", time.Now()); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(home, reportDirName, consentFileName)
	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	tampered := []byte(strings.Replace(string(raw), consentTextSHA("app"), "0", 1))
	if err := os.WriteFile(path, tampered, 0o600); err != nil {
		t.Fatal(err)
	}
	if ConsentValid(home, "app") {
		t.Fatal("tampered consent accepted")
	}
}

func TestDisableKeepsReceipts(t *testing.T) {
	home := t.TempDir()
	dir := filepath.Join(home, reportDirName)
	if err := WriteConsent(home, "app", time.Now()); err != nil {
		t.Fatal(err)
	}
	if err := writeCredential(home, "tok", time.Now().Add(time.Hour)); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, queueFileName), []byte(`{"entries":[]}`), 0o600); err != nil {
		t.Fatal(err)
	}
	state := `{"receipts":[{"fingerprint":"fp","state":"filed"}]}`
	if err := os.WriteFile(filepath.Join(dir, stateFileName), []byte(state), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := Disable(home); err != nil {
		t.Fatal(err)
	}
	for _, name := range []string{consentFileName, credentialFileName, queueFileName} {
		if _, err := os.Stat(filepath.Join(dir, name)); !os.IsNotExist(err) {
			t.Fatalf("%s survives Disable", name)
		}
	}
	if _, err := os.Stat(filepath.Join(dir, stateFileName)); err != nil {
		t.Fatal("receipts deleted by Disable")
	}
}
