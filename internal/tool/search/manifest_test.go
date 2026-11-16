package search

import (
	"bytes"
	"errors"
	"path/filepath"
	"slices"
	"strings"
	"testing"
)

func sampleManifest() manifest {
	return manifest{
		Start:       1700000100000000000,
		TrigramSize: 4096,
		Entries: []manifestEntry{
			{Path: "a.go", Size: 3, ModNS: 1700000000000000001, CtimeNS: 1700000000000000002, Indexed: true},
			{Path: "b/c.txt", Size: 0, ModNS: 5, CtimeNS: 6, Indexed: false},
		},
	}
}

func encodedSample(t *testing.T) []byte {
	t.Helper()
	data, err := encodeManifest(sampleManifest())
	if err != nil {
		t.Fatal(err)
	}
	return data
}

const handWrittenMagic = "dalgona search manifest 2 codesearch v1.2.0\n"

func handWrittenManifest() []byte {
	return []byte(handWrittenMagic +
		"\x17\x97\x9d\x15\x7e\xa0\xe8\x00" +
		"\x00\x00\x00\x00\x00\x00\x10\x00" +
		"\x00\x00\x00\x02" +
		"\x00\x00\x00\x04a.go" +
		"\x00\x00\x00\x00\x00\x00\x00\x03" +
		"\x17\x97\x9c\xfe\x36\x2a\x00\x01" +
		"\x17\x97\x9c\xfe\x36\x2a\x00\x02" +
		"\x01" +
		"\x00\x00\x00\x07b/c.txt" +
		"\x00\x00\x00\x00\x00\x00\x00\x00" +
		"\x00\x00\x00\x00\x00\x00\x00\x05" +
		"\x00\x00\x00\x00\x00\x00\x00\x06" +
		"\x00")
}

func TestManifestDecodesHandWrittenBytes(t *testing.T) {
	want := manifest{Start: 1700000100000000000, TrigramSize: 4096, Entries: []manifestEntry{
		{Path: "a.go", Size: 3, ModNS: 1700000000000000001, CtimeNS: 1700000000000000002, Indexed: true},
		{Path: "b/c.txt", Size: 0, ModNS: 5, CtimeNS: 6},
	}}
	got, err := decodeManifest(handWrittenManifest())
	if err != nil {
		t.Fatal(err)
	}
	if got.Start != want.Start || got.TrigramSize != want.TrigramSize || !slices.Equal(got.Entries, want.Entries) {
		t.Fatalf("decoded %+v, want %+v", got, want)
	}
}

func requireManifestError(t *testing.T, err error, want string) {
	t.Helper()
	if !errors.Is(err, errManifest) {
		t.Fatalf("error %v does not wrap errManifest", err)
	}
	if !strings.Contains(err.Error(), want) {
		t.Fatalf("error %q does not contain %q", err, want)
	}
}

func TestManifestRoundTrip(t *testing.T) {
	want := sampleManifest()
	got, err := decodeManifest(encodedSample(t))
	if err != nil {
		t.Fatal(err)
	}
	if got.Start != want.Start || got.TrigramSize != want.TrigramSize || len(got.Entries) != len(want.Entries) {
		t.Fatalf("header mismatch: got %+v, want %+v", got, want)
	}
	for i := range want.Entries {
		if got.Entries[i] != want.Entries[i] {
			t.Fatalf("entry %d = %+v, want %+v", i, got.Entries[i], want.Entries[i])
		}
	}
}

func TestManifestRejectsBadMagic(t *testing.T) {
	data := handWrittenManifest()
	data[0] = 'x'
	_, err := decodeManifest(data)
	requireManifestError(t, err, "bad magic")
}

func TestManifestRejectsOtherLibraryVersion(t *testing.T) {
	data := bytes.Replace(handWrittenManifest(), []byte("v1.2.0"), []byte("v1.3.0-rc.1"), 1)
	_, err := decodeManifest(data)
	requireManifestError(t, err, "bad magic")
}

func TestManifestRejectsUnsortedPaths(t *testing.T) {
	m := sampleManifest()
	m.Entries[0].Path, m.Entries[1].Path = "b", "a"
	_, err := encodeManifest(m)
	requireManifestError(t, err, "paths not sorted")
}

func TestManifestRejectsLongPath(t *testing.T) {
	m := sampleManifest()
	m.Entries = []manifestEntry{{Path: strings.Repeat("a", 4097)}}
	_, err := encodeManifest(m)
	requireManifestError(t, err, "path length")
}

func TestManifestRejectsTrailingBytes(t *testing.T) {
	data := append(handWrittenManifest(), 0)
	_, err := decodeManifest(data)
	requireManifestError(t, err, "trailing bytes")
}

func TestManifestRejectsTruncatedEntry(t *testing.T) {
	data := handWrittenManifest()
	_, err := decodeManifest(data[:len(data)-1])
	requireManifestError(t, err, "truncated")
}

func TestManifestLookup(t *testing.T) {
	m := sampleManifest()
	for _, want := range m.Entries {
		entry, ok := m.lookup(want.Path)
		if !ok || entry != want {
			t.Fatalf("lookup %q = %+v, %t; want %+v", want.Path, entry, ok, want)
		}
	}
	for _, path := range []string{"0", "a.gz", "missing", "z"} {
		if entry, ok := m.lookup(path); ok {
			t.Fatalf("lookup %q found %+v", path, entry)
		}
	}
}

func TestManifestWriteReadFile(t *testing.T) {
	path := filepath.Join(t.TempDir(), "manifest")
	if err := writeManifest(path, sampleManifest()); err != nil {
		t.Fatal(err)
	}
	got, err := readManifest(path)
	if err != nil {
		t.Fatal(err)
	}
	if len(got.Entries) != 2 || got.Entries[1].Path != "b/c.txt" {
		t.Fatalf("read back %+v", got)
	}
}

func setCount(data []byte, count byte) {
	data[len(manifestMagic)+19] = count
}

func longPathManifest(t *testing.T) []byte {
	t.Helper()
	data, err := encodeManifest(manifest{Start: 1, TrigramSize: 2, Entries: []manifestEntry{
		{Path: "a" + strings.Repeat("x", 199)},
		{Path: "b" + strings.Repeat("y", 199)},
	}})
	if err != nil {
		t.Fatal(err)
	}
	return data
}

func TestManifestRejectsDamagedBytes(t *testing.T) {
	tests := []struct {
		name string
		data func(t *testing.T) []byte
		want string
	}{
		{"bad indexed byte", func(t *testing.T) []byte {
			data := handWrittenManifest()
			data[len(data)-1] = 2
			return data
		}, "bad indexed byte"},
		{"inflated count past the body", func(t *testing.T) []byte {
			data := handWrittenManifest()
			setCount(data, 3)
			return data
		}, "count mismatch"},
		{"inflated count inside the body", func(t *testing.T) []byte {
			data := longPathManifest(t)
			setCount(data, 3)
			return data
		}, "count mismatch"},
		{"deflated count", func(t *testing.T) []byte {
			data := handWrittenManifest()
			setCount(data, 1)
			return data
		}, "count mismatch"},
		{"NUL in path", func(t *testing.T) []byte {
			data := handWrittenManifest()
			data[bytes.Index(data, []byte("a.go"))] = 0
			return data
		}, "NUL in path"},
		{"negative trigram size", func(t *testing.T) []byte {
			data := handWrittenManifest()
			for i := range 8 {
				data[len(manifestMagic)+8+i] = 0xff
			}
			return data
		}, "trigram size"},
		{"header shorter than the header size", func(t *testing.T) []byte {
			return handWrittenManifest()[:manifestHeaderBytes-1]
		}, "truncated"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			_, err := decodeManifest(tt.data(t))
			requireManifestError(t, err, tt.want)
		})
	}
}

func TestManifestRejectsBadEncodeInput(t *testing.T) {
	tests := []struct {
		name     string
		manifest manifest
		want     string
	}{
		{"empty path", manifest{Entries: []manifestEntry{{Path: ""}}}, "empty path"},
		{"NUL in path", manifest{Entries: []manifestEntry{{Path: "a\x00b"}}}, "NUL in path"},
		{"negative trigram size", manifest{TrigramSize: -1}, "trigram size"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			_, err := encodeManifest(tt.manifest)
			requireManifestError(t, err, tt.want)
		})
	}
}
