//go:build gui

package desktop

import (
	"errors"
	"strings"
	"testing"
	"unicode/utf16"

	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

func TestFileSystemPathAcceptsOnlyBoundedAbsolutePaths(t *testing.T) {
	cases := map[string]struct {
		units []uint16
		want  string
		err   error
	}{
		"drive path":         {wideText(`C:\Users\u\a b.txt`), `C:\Users\u\a b.txt`, nil},
		"share path":         {wideText(`\\server\share\a.txt`), `\\server\share\a.txt`, nil},
		"unpaired surrogate": {[]uint16{'C', ':', '\\', 0xD800, '.', 't', 0}, "C:\\\xed\xa0\x80.t", nil},
		"longest path":       {wideText(`C:\` + strings.Repeat("a", 32767-3)), `C:\` + strings.Repeat("a", 32767-3), nil},
		"unterminated":       {wideText(`C:\` + strings.Repeat("a", 32768-3)), "", platform.ErrNotLocalFile},
		"relative path":      {wideText(`docs\a.txt`), "", platform.ErrNotLocalFile},
		"empty path":         {[]uint16{0}, "", platform.ErrNotLocalFile},
	}
	for name, c := range cases {
		got, err := fileSystemPath(&c.units[0])
		if got != c.want || !errors.Is(err, c.err) {
			t.Errorf("%s: path %.40q, error %v; want %.40q, %v", name, got, err, c.want, c.err)
		}
	}
}

func wideText(text string) []uint16 {
	return append(utf16.Encode([]rune(text)), 0)
}
