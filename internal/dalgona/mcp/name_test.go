package mcp

import (
	"crypto/sha256"
	"encoding/hex"
	"math/rand/v2"
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

var nameFragments = []string{"a", "Z", "0", "_", "-", ".", "/", ":", " ", "@", "é", "中", "😀"}

var nameLengths = []int{1, 2, 5, 55, 62, 63, 64, 65, 100, 200}

func randomNamePart(r *rand.Rand) string {
	target := nameLengths[r.IntN(len(nameLengths))]
	var b strings.Builder
	for b.Len() < target {
		b.WriteString(nameFragments[r.IntN(len(nameFragments))])
	}
	return b.String()
}

func checkToolName(t *testing.T, seen map[string]string, server, tool string) {
	t.Helper()
	joined := server + "__" + tool
	got := toolName(server, tool)
	if len(got) > maxToolName {
		t.Fatalf("toolName(%q, %q) = %q, %d bytes over the 64-byte cap", server, tool, got, len(got))
	}
	if !safeNamePattern.MatchString(got) {
		t.Fatalf("toolName(%q, %q) = %q, outside the provider set", server, tool, got)
	}
	if again := toolName(server, tool); again != got {
		t.Fatalf("toolName(%q, %q) changed from %q to %q", server, tool, got, again)
	}
	if prev, dup := seen[got]; dup && prev != joined {
		t.Fatalf("toolName collision: %q and %q both give %q", prev, joined, got)
	}
	seen[got] = joined
}

func TestToolNameHoldsAcrossRandomNames(t *testing.T) {
	seen := make(map[string]string)
	edges := [][2]string{
		{"", "echo"},
		{"s", ""},
		{"-x", "y"},
		{"_ok", "fine"},
		{strings.Repeat("a", 200), "b"},
		{"srv", strings.Repeat("😀", 50)},
		{"plug:srv", "a/b.c:d e@f"},
		{"a", strings.Repeat("é", 100)},
	}
	for _, edge := range edges {
		checkToolName(t, seen, edge[0], edge[1])
	}
	r := rand.New(rand.NewPCG(0x6d63706e616d65, 0x746f6f6c303631))
	for range 300 {
		checkToolName(t, seen, randomNamePart(r), randomNamePart(r))
	}
}
