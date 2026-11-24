//go:build gui

package desktop

import (
	"errors"
	"slices"
	"testing"

	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

func TestFileDialogOptionsFollowTheRequest(t *testing.T) {
	text := []platform.FileFilter{{Name: "Text", Extensions: []string{"txt"}}}
	cases := map[string]struct {
		req  platform.DialogRequest
		want uintptr
	}{
		"open files":       {platform.DialogRequest{Kind: platform.DialogOpenFiles, Title: "Attach files", Filters: text}, 0x8 | 0x40 | 0x800 | 0x200 | 0x1000},
		"choose folder":    {platform.DialogRequest{Kind: platform.DialogChooseFolder, Title: "Choose workspace folder"}, 0x8 | 0x40 | 0x800 | 0x20},
		"save":             {platform.DialogRequest{Kind: platform.DialogSaveFile, Title: "Export document", SuggestedName: "notes"}, 0x8 | 0x40 | 0x800 | 0x2 | 0x8000 | 0x10000},
		"save with filter": {platform.DialogRequest{Kind: platform.DialogSaveFile, Title: "Export document", SuggestedName: "notes.txt", Filters: text}, 0x8 | 0x40 | 0x800 | 0x2 | 0x4 | 0x8000 | 0x10000},
	}
	for name, c := range cases {
		if got := fileDialogOptions(c.req); got != c.want {
			t.Errorf("%s: options %#x, want %#x", name, got, c.want)
		}
	}
}

func TestFilterPatternNamesEachExtension(t *testing.T) {
	cases := map[string][]string{
		"*.md":         {"md"},
		"*.png;*.jpeg": {"png", "jpeg"},
	}
	for want, extensions := range cases {
		if got := filterPattern(extensions); got != want {
			t.Errorf("filterPattern(%q) = %q, want %q", extensions, got, want)
		}
	}
}

func TestSheetPathsAreAbsoluteLocalPaths(t *testing.T) {
	got, err := splitPaths([]byte("/Users/u/a b.txt\x00/Users/u/\xed\x95\x9c.png"))
	if want := []string{"/Users/u/a b.txt", "/Users/u/\xed\x95\x9c.png"}; err != nil || !slices.Equal(got, want) {
		t.Fatalf("paths %q, error %v; want %q", got, err, want)
	}
	refused := map[string]string{
		"no selection":    "",
		"relative path":   "notes.md",
		"trailing empty":  "/Users/u/a\x00",
		"empty between":   "/Users/u/a\x00\x00/Users/u/b",
		"file URL string": "file:///Users/u/a",
	}
	for name, encoded := range refused {
		if paths, err := splitPaths([]byte(encoded)); !errors.Is(err, platform.ErrNotLocalFile) {
			t.Errorf("%s: paths %q, error %v; want ErrNotLocalFile", name, paths, err)
		}
	}
}
