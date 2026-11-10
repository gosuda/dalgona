package runner

import (
	"strings"
	"testing"
)

func TestStagingPath(t *testing.T) {
	cases := []struct {
		dest string
		dir  string
	}{
		{"/a/b/f", "/a/b/"},
		{`C:\dir\f`, `C:\dir\`},
		{"/a/b\\c", "/a/"},
		{"C:/x/y", "C:/x/"},
		{"f", ""},
		{"/a/b/", "/a/b/"},
		{`C:\dir\`, `C:\dir\`},
	}
	for _, tc := range cases {
		staged, err := stagingPath(tc.dest)
		if err != nil {
			t.Fatalf("stagingPath(%q) failed: %v", tc.dest, err)
		}
		if !strings.HasPrefix(staged, tc.dir) {
			t.Fatalf("stagingPath(%q) = %q, want prefix %q", tc.dest, staged, tc.dir)
		}
		base := staged[len(tc.dir):]
		if len(base) != 33 || !strings.HasPrefix(base, ".dalgona-partial-") {
			t.Fatalf("stagingPath(%q) basename %q is %d bytes, want 33", tc.dest, base, len(base))
		}
		if strings.ContainsAny(base, "/\\") {
			t.Fatalf("stagingPath(%q) basename %q holds a separator", tc.dest, base)
		}
	}
}
