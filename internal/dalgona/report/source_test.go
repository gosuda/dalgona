package report

import (
	"bytes"
	"context"
	"strings"
	"testing"
	"time"
)

func TestGHTokenParse(t *testing.T) {
	for name, tc := range map[string]struct {
		raw  string
		want string
		ok   bool
	}{
		"newline":      {"tok123\n", "tok123", true},
		"crlf":         {"tok123\r\n", "tok123", true},
		"no newline":   {"tok123", "tok123", true},
		"empty":        {"", "", false},
		"newline only": {"\n", "", false},
		"embedded nl":  {"a\nb\n", "", false},
		"control":      {"a\x01b\n", "", false},
		"oversized":    {strings.Repeat("a", ghTokenMaxBytes+1) + "\n", "", false},
	} {
		t.Run(name, func(t *testing.T) {
			got, err := parseGHToken(tc.raw)
			if tc.ok != (err == nil) || got != tc.want {
				t.Fatalf("parseGHToken(%q) = %q, %v; want %q, ok=%v", tc.raw, got, err, tc.want, tc.ok)
			}
		})
	}
}

func TestGHSourceCachesWithinRun(t *testing.T) {
	calls := 0
	source := &GHSource{
		lookPath: func(string) (string, error) { return "/bin/gh", nil },
		run: func(context.Context, string) ([]byte, error) {
			calls++
			return []byte("tok\n"), nil
		},
	}
	ctx := context.Background()
	first, err := source.Credential(ctx)
	if err != nil {
		t.Fatal(err)
	}
	second, err := source.Credential(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if calls != 1 || first.Access != "tok" || second.Access != "tok" {
		t.Fatalf("calls = %d, access = %q/%q", calls, first.Access, second.Access)
	}
	source.BeginRun()
	if _, err := source.Credential(ctx); err != nil {
		t.Fatal(err)
	}
	if calls != 2 {
		t.Fatalf("calls = %d, want 2 after BeginRun", calls)
	}
}

func TestGHSourceEnvStripped(t *testing.T) {
	env := strippedGHEnv([]string{"GH_TOKEN=x", "GITHUB_TOKEN=y", "GH_ENTERPRISE_TOKEN=z", "PATH=/bin", "HOME=/h"})
	joined := strings.Join(env, "|")
	for _, leaked := range []string{"GH_TOKEN", "GITHUB_TOKEN", "GH_ENTERPRISE_TOKEN"} {
		if strings.Contains(joined, leaked+"=") {
			t.Fatalf("env leaked %s: %v", leaked, env)
		}
	}
	if !strings.Contains(joined, "PATH=/bin") {
		t.Fatal("env lost PATH")
	}
}

func TestAppSourceExpired(t *testing.T) {
	source := &AppSource{access: "", now: time.Now}
	_, err := source.Credential(context.Background())
	if err == nil || !strings.Contains(err.Error(), "failure reporting authorization expired; run dalgona failures reporting login") {
		t.Fatalf("error = %v, want expired message", err)
	}
}

func TestOpenConsentedDisabledSilent(t *testing.T) {
	var notice bytes.Buffer
	opened, err := OpenConsented(context.Background(), OpenParams{Home: t.TempDir(), Method: "gh", Enabled: false, Notice: &notice})
	if err != nil || opened != nil || notice.Len() != 0 {
		t.Fatalf("disabled: opened=%v err=%v notice=%q", opened, err, notice.String())
	}
}

func TestOpenConsentedOverriddenHome(t *testing.T) {
	var notice bytes.Buffer
	opened, err := OpenConsented(context.Background(), OpenParams{Home: t.TempDir(), Method: "gh", Enabled: true, HomeOverridden: true, Notice: &notice})
	if err != nil || opened != nil || !strings.Contains(notice.String(), "off for an overridden home") {
		t.Fatalf("overridden: opened=%v err=%v notice=%q", opened, err, notice.String())
	}
}

func TestOpenConsentedConsentGates(t *testing.T) {
	home := t.TempDir()
	var notice bytes.Buffer
	params := OpenParams{Home: home, Method: "gh", Enabled: true, Notice: &notice}
	opened, err := OpenConsented(context.Background(), params)
	if err != nil || opened != nil || !strings.Contains(notice.String(), "automatic failure reporting is off: consent is required") {
		t.Fatalf("no consent: opened=%v err=%v notice=%q", opened, err, notice.String())
	}
	if writeErr := WriteConsent(home, "app", time.Now()); writeErr != nil {
		t.Fatal(writeErr)
	}
	notice.Reset()
	opened, err = OpenConsented(context.Background(), params)
	if err != nil || opened != nil || !strings.Contains(notice.String(), "authentication changed") {
		t.Fatalf("method switch: opened=%v err=%v notice=%q", opened, err, notice.String())
	}
}

func TestOpenConsentedValidConsent(t *testing.T) {
	home := t.TempDir()
	if err := WriteConsent(home, "gh", time.Now()); err != nil {
		t.Fatal(err)
	}
	opened, err := OpenConsented(context.Background(), OpenParams{Home: home, Method: "gh", Enabled: true, Notice: &bytes.Buffer{}})
	if err != nil || opened == nil {
		t.Fatalf("valid consent: opened=%v err=%v", opened, err)
	}
	defer func() { _ = opened.Close(context.Background()) }()
}
