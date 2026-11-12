package mcp

import (
	"crypto/sha256"
	"encoding/hex"
	"regexp"
	"strings"
	"testing"
)

var safeNamePattern = regexp.MustCompile(`^[A-Za-z_][A-Za-z0-9_-]{0,63}$`)

var hashedSuffix = regexp.MustCompile(`_[0-9a-f]{8}$`)

func hashOf(joined string) string {
	sum := sha256.Sum256([]byte(joined))
	return hex.EncodeToString(sum[:4])
}

func TestToolNameIsProviderSafe(t *testing.T) {
	long := strings.Repeat("a", 100)
	cases := []struct {
		name   string
		server string
		tool   string
		want   string
	}{
		{name: "unchanged", server: "web", tool: "echo", want: "web__echo"},
		{name: "colon in server", server: "plug:srv", tool: "echo", want: "plug_srv__echo_" + hashOf("plug:srv__echo")},
		{name: "leading digit", server: "1password", tool: "x", want: "_1password__x_" + hashOf("1password__x")},
		{name: "long tool", server: "web", tool: long, want: "web__" + strings.Repeat("a", 50) + "_" + hashOf("web__"+long)},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := toolName(tc.server, tc.tool)
			if got != tc.want {
				t.Fatalf("toolName(%q, %q) = %q, want %q", tc.server, tc.tool, got, tc.want)
			}
			if !safeNamePattern.MatchString(got) {
				t.Fatalf("toolName(%q, %q) = %q does not match %s", tc.server, tc.tool, got, safeNamePattern)
			}
		})
	}

	for _, tc := range []struct{ server, tool string }{{"plug:srv", "echo"}, {"1password", "x"}, {"web", long}} {
		if got := toolName(tc.server, tc.tool); !hashedSuffix.MatchString(got) {
			t.Fatalf("toolName(%q, %q) = %q, want a trailing _ and 8 hex digits", tc.server, tc.tool, got)
		}
	}

	if a, b := toolName("s", "a.b"), toolName("s", "a:b"); a == b {
		t.Fatalf("toolName(s, a.b) and toolName(s, a:b) both give %q", a)
	}
	if a, b := toolName("plug:srv", "echo"), toolName("plug:srv", "echo"); a != b {
		t.Fatalf("toolName is not repeatable: %q then %q", a, b)
	}
}
