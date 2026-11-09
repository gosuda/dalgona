package app

import (
	"bytes"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

func TestLoginRunnerServiceAPIKeyStoresTheRemoteToken(t *testing.T) {
	for _, service := range []string{"sprites", "sandbox0", "cloudflare"} {
		t.Run(service, func(t *testing.T) {
			home := t.TempDir()
			var stdout, stderr bytes.Buffer
			env := CLIEnv{Product: config.ProductDalgona, Home: home, Stdout: &stdout, Stderr: &stderr}
			if code := RunCLI(t.Context(), env, []string{"login", service, "--api-key", "tok-1"}); code != 0 {
				t.Fatalf("login %s exit %d: %s", service, code, stderr.String())
			}
			if got := apiKeyIn(t, home, service); got != "tok-1" {
				t.Fatalf("%s key = %q, want tok-1", service, got)
			}
		})
	}
}

func TestRemoteLoginRunnerServiceAPIKeyStoresItInTheBox(t *testing.T) {
	box := newServedBox(t)
	clientHome := t.TempDir()
	var stdout, stderr bytes.Buffer
	env := CLIEnv{Product: config.ProductDalgona, Home: clientHome, Remote: "box", Dial: box.dial, Stdout: &stdout, Stderr: &stderr}
	if code := RunCLI(t.Context(), env, []string{"login", "sprites", "--api-key", "tok-1"}); code != 0 {
		t.Fatalf("remote sprites login exit %d: %s", code, stderr.String())
	}
	if got := apiKeyIn(t, box.home, "sprites"); got != "tok-1" {
		t.Fatalf("box sprites key = %q, want tok-1", got)
	}
	if got := apiKeyIn(t, clientHome, "sprites"); got != "" {
		t.Fatalf("client sprites key = %q, want empty", got)
	}
}

func TestLoginAPIKeyStillRequiresAKeyProvider(t *testing.T) {
	var stderr bytes.Buffer
	env := CLIEnv{Product: config.ProductDalgona, Home: t.TempDir(), Stdout: &bytes.Buffer{}, Stderr: &stderr}
	if code := RunCLI(t.Context(), env, []string{"login", "antigravity", "--api-key", "k"}); code != 1 {
		t.Fatalf("antigravity --api-key exit %d", code)
	}
	if !strings.Contains(stderr.String(), "--api-key works only for") {
		t.Fatalf("stderr %q", stderr.String())
	}
}
