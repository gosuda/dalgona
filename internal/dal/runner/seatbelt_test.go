//go:build unix

package runner

import (
	"io/fs"
	"slices"
	"testing"
)

func linkResolver(links map[string]string) func(string) (string, error) {
	return func(path string) (string, error) {
		target, ok := links[path]
		if !ok {
			return "", fs.ErrNotExist
		}
		return target, nil
	}
}

func TestSeatbeltProfileEscapesRootsAndDeniedSockets(t *testing.T) {
	got := seatbeltProfile([]string{`/work/a"b`, `/cache/c\d`}, []string{`/home/q"s\t.sock`})
	want := `(version 1) (allow default)` +
		` (deny network-outbound (remote unix-socket (path-literal "/home/q\"s\\t.sock")))` +
		` (deny file-write*)` +
		` (allow file-write* (subpath "/work/a\"b"))` +
		` (allow file-write* (subpath "/cache/c\\d"))` +
		` (allow file-write* (literal "/dev/null") (literal "/dev/zero") (regex #"^/dev/tty") (regex #"^/dev/fd/"))`
	if got != want {
		t.Fatalf("profile =\n%s\nwant\n%s", got, want)
	}
}

func TestDeniedSocketSpellingsCoverCleanedResolvedAndCanonicalParent(t *testing.T) {
	resolve := linkResolver(map[string]string{
		"/home/user/../user/.dalgona/dalgona.sock": "/data/user/.dalgona/dalgona.sock",
		"/data/user/.dalgona":                      "/volume/data/user/.dalgona",
	})
	got, err := deniedSocketSpellings([]string{"/home/user/../user/.dalgona/dalgona.sock"}, resolve)
	if err != nil {
		t.Fatal(err)
	}
	want := []string{
		"/data/user/.dalgona/dalgona.sock",
		"/home/user/.dalgona/dalgona.sock",
		"/volume/data/user/.dalgona/dalgona.sock",
	}
	if !slices.Equal(got, want) {
		t.Fatalf("spellings = %q, want %q", got, want)
	}
}

func TestDeniedSocketSpellingsResolveMissingSocketThroughRealParent(t *testing.T) {
	resolve := linkResolver(map[string]string{
		"/var/run":         "/private/var/run",
		"/private/var/run": "/private/var/run",
	})
	got, err := deniedSocketSpellings([]string{"/var/run/dalgona.sock"}, resolve)
	if err != nil {
		t.Fatal(err)
	}
	want := []string{"/private/var/run/dalgona.sock", "/var/run/dalgona.sock"}
	if !slices.Equal(got, want) {
		t.Fatalf("spellings = %q, want %q", got, want)
	}
}

func TestDeniedSocketSpellingsFailWhenParentDoesNotResolve(t *testing.T) {
	got, err := deniedSocketSpellings([]string{"/missing/dir/dalgona.sock"}, linkResolver(nil))
	if err == nil {
		t.Fatalf("spellings = %q, want an error for an unresolved parent", got)
	}
}

func TestDeniedSocketSpellingsRefuseControlCharacter(t *testing.T) {
	resolve := linkResolver(map[string]string{"/tmp": "/tmp"})
	got, err := deniedSocketSpellings([]string{"/tmp/a\nb.sock"}, resolve)
	if err == nil {
		t.Fatalf("spellings = %q, want an error for a control character", got)
	}
}
