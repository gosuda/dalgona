//go:build !windows

package platform

import (
	"errors"
	"testing"
)

func TestCheckOpenURLAcceptsOnlyDeclaredSchemes(t *testing.T) {
	accepted := map[string][]string{
		"https://github.com/login/device?code=AB-12": {"https"},
		"HTTPS://example.com/a%20b":                  {"https"},
		"cursor://anysphere.cursor-deeplink/mcp/x":   {"cursor"},
	}
	for raw, schemes := range accepted {
		got, err := checkOpenURL(raw, schemes)
		if err != nil || got != raw {
			t.Errorf("checkOpenURL(%q, %v) = %q, %v; want the exact URL", raw, schemes, got, err)
		}
	}
	refused := map[string][]string{
		"http://example.com":            {"https"},
		"file:///etc/passwd":            {"file", "https"},
		"https:///no-host":              {"https"},
		"https://example.com/a b":       {"https"},
		"https://example.com/\nheader":  {"https"},
		"javascript:alert(1)":           {"https"},
		"/usr/bin/xterm":                {"https"},
		"https://example.com/\x7fdel":   {"https"},
		"ssh://host":                    nil,
		"vscode://file/home/u/a.go:3:1": {"cursor"},
	}
	for raw, schemes := range refused {
		if got, err := checkOpenURL(raw, schemes); !errors.Is(err, ErrOpenRefused) {
			t.Errorf("checkOpenURL(%q, %v) = %q, %v; want ErrOpenRefused", raw, schemes, got, err)
		}
	}
}

func TestCheckOpenPathRequiresAnAbsolutePath(t *testing.T) {
	if err := checkOpenPath("/home/u/My Notes/한글.md"); err != nil {
		t.Fatalf("absolute path with spaces and Hangul refused: %v", err)
	}
	for _, p := range []string{"notes.md", "-R", "/home/u/a\x00b", ""} {
		if err := checkOpenPath(p); !errors.Is(err, ErrNotAbsolute) {
			t.Errorf("checkOpenPath(%q) = %v, want ErrNotAbsolute", p, err)
		}
	}
}

func TestLinkSchemesAreWebAndMail(t *testing.T) {
	for _, raw := range []string{"https://example.com/x", "http://example.com", "mailto:team@example.com?subject=Hi"} {
		if _, err := checkOpenURL(raw, linkSchemes); err != nil {
			t.Errorf("link %q refused: %v", raw, err)
		}
	}
	for _, raw := range []string{"ftp://example.com", "javascript:alert(1)", "file:///etc/passwd", "cursor://x", "data:text/html,hi"} {
		if _, err := checkOpenURL(raw, linkSchemes); !errors.Is(err, ErrOpenRefused) {
			t.Errorf("link %q accepted: %v", raw, err)
		}
	}
}
