//go:build !windows

package platform

import (
	"errors"
	"slices"
	"testing"
)

func TestLocalFileDecodesEscapedLocalPaths(t *testing.T) {
	cases := map[string]string{
		"file:///home/user/My%20Notes.md":           "/home/user/My Notes.md",
		"file:///tmp/%ED%95%9C%EA%B8%80.txt":        "/tmp/한글.txt",
		"file:///tmp/한글 raw.txt":                    "/tmp/한글 raw.txt",
		"file://localhost/tmp/a%23b%3Fc%25d":        "/tmp/a#b?c%d",
		"FILE:///tmp/upper-scheme":                  "/tmp/upper-scheme",
		"file:/tmp/single-slash":                    "/tmp/single-slash",
		"file:///tmp/emoji-%F0%9F%91%A9%E2%80%8D.p": "/tmp/emoji-\U0001F469\u200D.p",
	}
	for uri, want := range cases {
		got, err := LocalFile(uri)
		if err != nil || got != want {
			t.Errorf("LocalFile(%q) = %q, %v; want %q", uri, got, err, want)
		}
	}
}

func TestLocalFileRefusesRemoteAndAmbiguousURLs(t *testing.T) {
	for _, uri := range []string{
		"https://example.com/report.pdf",
		"smb://server/share/file",
		"file://server/share/file",
		"file://localhost:80/tmp/x",
		"file://user@localhost/tmp/x",
		"file:relative/path",
		"file:///tmp/a#fragment",
		"file:///tmp/a?query",
		"file:///tmp/a?",
		"file:///tmp/a%2Fb",
		"file:///tmp/a%00b",
		"file://",
		"/tmp/plain-path",
		"",
	} {
		if got, err := LocalFile(uri); !errors.Is(err, ErrNotLocalFile) {
			t.Errorf("LocalFile(%q) = %q, %v; want ErrNotLocalFile", uri, got, err)
		}
	}
}

func TestParseURIListPreservesDropOrder(t *testing.T) {
	list := "# dropped by the file manager\r\nfile:///tmp/z%20last\r\nfile:///tmp/a\r\n\r\nfile:///tmp/%EC%A4%91\n"
	got, err := ParseURIList(list)
	want := []string{"/tmp/z last", "/tmp/a", "/tmp/중"}
	if err != nil || !slices.Equal(got, want) {
		t.Fatalf("ParseURIList = %q, %v; want %q", got, err, want)
	}
}

func TestParseURIListRejectsTheWholeListForOneRemoteEntry(t *testing.T) {
	for _, list := range []string{
		"file:///tmp/a\r\nhttps://example.com/b\r\n",
		"# only a comment\r\n",
		"",
	} {
		if got, err := ParseURIList(list); !errors.Is(err, ErrNotLocalFile) || got != nil {
			t.Errorf("ParseURIList(%q) = %q, %v; want ErrNotLocalFile and no paths", list, got, err)
		}
	}
}
