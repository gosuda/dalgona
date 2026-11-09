package prompt_test

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/prompt"
)

func TestMentionsAttachWorkspaceFile(t *testing.T) {
	t.Parallel()
	cwd := t.TempDir()
	writeTestFile(t, filepath.Join(cwd, "go.mod"), "module example.com/project")
	mentions := prompt.Mentions("explain @go.mod", cwd)
	if len(mentions) != 1 {
		t.Fatalf("mention count = %d, want 1", len(mentions))
	}
	if mentions[0].Path != "go.mod" || mentions[0].Text != "module example.com/project" {
		t.Fatalf("mention = %#v", mentions[0])
	}
	if got := prompt.Mentions("mail@go.mod", cwd); len(got) != 0 {
		t.Fatalf("email-like token produced mentions: %#v", got)
	}
}

func TestMentionsRejectOutsidePath(t *testing.T) {
	t.Parallel()
	base := t.TempDir()
	cwd := filepath.Join(base, "cwd")
	writeTestFile(t, filepath.Join(cwd, "inside.txt"), "inside")
	writeTestFile(t, filepath.Join(base, "outside.txt"), "outside")
	if got := prompt.Mentions("explain @inside.txt", cwd); len(got) != 1 || got[0].Text != "inside" {
		t.Fatalf("inside control produced mentions: %#v", got)
	}
	if got := prompt.Mentions("explain @../outside.txt", cwd); len(got) != 0 {
		t.Fatalf("outside path produced mentions: %#v", got)
	}
}

func TestMentionsRejectEscapingSymlink(t *testing.T) {
	t.Parallel()
	base := t.TempDir()
	cwd := filepath.Join(base, "cwd")
	outside := filepath.Join(base, "outside.txt")
	writeTestFile(t, outside, "outside")
	if err := os.MkdirAll(cwd, 0o755); err != nil {
		t.Fatalf("create cwd: %v", err)
	}
	if err := os.Symlink(outside, filepath.Join(cwd, "link.txt")); err != nil {
		t.Skipf("symlinks unavailable: %v", err)
	}
	if got := prompt.Mentions("explain @link.txt", cwd); len(got) != 0 {
		t.Fatalf("escaping symlink produced mentions: %#v", got)
	}
}

func TestMentionsProjectLargeText(t *testing.T) {
	t.Parallel()
	cwd := t.TempDir()
	writeTestFile(t, filepath.Join(cwd, "large.txt"), strings.Repeat("x", 70*1024))
	mentions := prompt.Mentions("@large.txt", cwd)
	if len(mentions) != 1 {
		t.Fatalf("mention count = %d, want 1", len(mentions))
	}
	text := mentions[0].Text
	if len(text) >= 70*1024 {
		t.Fatalf("large mention was not projected: %d bytes", len(text))
	}
	if !strings.HasPrefix(text, strings.Repeat("x", 4096)) {
		t.Fatal("projection did not keep the file head")
	}
	if !strings.Contains(text, "[output truncated: 71680 bytes total; full output: ") {
		t.Fatalf("projection lacks total-size notice: %s", text)
	}
}

func TestMentionsMarkBinaryFile(t *testing.T) {
	t.Parallel()
	tests := []struct {
		name string
		data []byte
	}{
		{name: "nul", data: []byte{'a', 0, 'b'}},
		{name: "invalid UTF-8", data: []byte{0xff, 0x01}},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			cwd := t.TempDir()
			path := filepath.Join(cwd, "blob.bin")
			if err := os.WriteFile(path, test.data, 0o644); err != nil {
				t.Fatalf("write binary file: %v", err)
			}
			mentions := prompt.Mentions("@blob.bin", cwd)
			if len(mentions) != 1 || mentions[0].Text != "[binary file skipped]" {
				t.Fatalf("binary mentions = %#v", mentions)
			}
		})
	}
}
