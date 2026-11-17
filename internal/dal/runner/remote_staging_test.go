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
		{"/f", "/"},
		{`C:\f`, `C:\`},
		{`\\server\share\dir\f`, `\\server\share\dir\`},
		{`a\b`, `a\`},
		{"C:", ""},
	}
	for _, tc := range cases {
		assertStagingPath(t, tc.dest, tc.dir)
	}
}

func assertStagingPath(t *testing.T, dest, dir string) {
	t.Helper()
	staged, err := stagingPath(dest)
	if err != nil {
		t.Fatalf("stagingPath(%q) failed: %v", dest, err)
	}
	if !strings.HasPrefix(staged, dir) {
		t.Fatalf("stagingPath(%q) = %q, want prefix %q", dest, staged, dir)
	}
	base := staged[len(dir):]
	if len(base) != 33 || !strings.HasPrefix(base, ".dalgona-partial-") {
		t.Fatalf("stagingPath(%q) basename %q is %d bytes, want 33", dest, base, len(base))
	}
	if strings.ContainsAny(base, "/\\") {
		t.Fatalf("stagingPath(%q) basename %q holds a separator", dest, base)
	}
}

func TestStagingPathEmpty(t *testing.T) {
	if _, err := stagingPath(""); err == nil {
		t.Fatal("stagingPath(\"\") succeeded, want error")
	}
}

func TestStagingPathUnique(t *testing.T) {
	first, err := stagingPath("/a/b/f")
	if err != nil {
		t.Fatal(err)
	}
	second, stageErr := stagingPath("/a/b/f")
	if stageErr != nil {
		t.Fatal(stageErr)
	}
	if first == second {
		t.Fatal("two uploads drew the same staging name")
	}
}
