package snapshot

import (
	"bytes"
	"encoding/json/jsontext"
	"fmt"
	"strconv"
	"strings"
	"testing"
)

func TestMetadataUsesJSONControlEscapes(t *testing.T) {
	data := []byte("body")
	type escapeCase struct {
		name       string
		path       string
		wantSubstr string
	}
	var cases []escapeCase
	for value := range 0x20 {
		b := byte(value)
		want := jsonControlEscape(b)
		cases = append(cases, escapeCase{
			name:       fmt.Sprintf("control_%02x", b),
			path:       "a" + string([]byte{b}) + "b",
			wantSubstr: want,
		})
	}

	cases = append(cases,
		escapeCase{name: "quote", path: "a\"b", wantSubstr: `\"`},
		escapeCase{name: "backslash", path: "a\\b", wantSubstr: `\\`},
		escapeCase{name: "line_separator", path: "a\u2028b", wantSubstr: "\u2028"},
		escapeCase{name: "multibyte_accent", path: "a\u00e9b", wantSubstr: "\u00e9"},
		escapeCase{name: "emoji_4byte", path: "a\U0001F600b", wantSubstr: "\U0001F600"},
	)

	for idx, tc := range cases {
		ref := "r0123456789abcdef0123456789abcdef." + strconv.FormatUint(uint64(idx+1), 36)
		entry := Entry{
			Ref:      ref,
			Path:     tc.path,
			Blob:     Digest(data),
			Revision: uint64(idx + 1),
			Epoch:    1,
		}
		meta := entry.Meta()

		decoder := jsontext.NewDecoder(bytes.NewReader(meta))
		if _, err := decoder.ReadValue(); err != nil {
			t.Fatalf("%s: metadata is not valid JSON: %v\n%s", tc.name, err, meta)
		}
		if !strings.Contains(string(meta), tc.wantSubstr) {
			t.Fatalf("%s: metadata missing expected escape %q in %s", tc.name, tc.wantSubstr, meta)
		}
		rebuilt := New()
		if err := rebuilt.Restore(meta, data); err != nil {
			t.Fatalf("%s: restore failed: %v", tc.name, err)
		}
		got, err := rebuilt.Get(entry.Ref)
		if err != nil || got.Path != entry.Path {
			t.Fatalf("%s: round trip mismatch: got %q (err %v), want %q", tc.name, got.Path, err, entry.Path)
		}
	}
}

func jsonControlEscape(b byte) string {
	switch b {
	case '\b':
		return `\b`
	case '\t':
		return `\t`
	case '\n':
		return `\n`
	case '\f':
		return `\f`
	case '\r':
		return `\r`
	default:
		return fmt.Sprintf(`\u00%02x`, b)
	}
}

func TestHeaderPathUsesJSONEscapes(t *testing.T) {
	cases := []struct {
		input string
		want  string
	}{
		{"a\x01b", `"a\u0001b"`},
		{"plain/f.txt", "plain/f.txt"},
		{"", `""`},
		{" a", `" a"`},
		{"a ", `"a "`},
		{"\"a", `"\"a"`},
		{"a\"b", "a\"b"},
		{"a@b", `"a@b"`},
		{"a#b", `"a#b"`},
		{"a]b", `"a]b"`},
		{"a\nb", `"a\nb"`},
		{"a\rb", `"a\rb"`},
		{"a\tb", `"a\tb"`},
		{"a\x7fb", "\"a\x7fb\""},
		{"nested/path/to/file.go", "nested/path/to/file.go"},
	}
	for _, tc := range cases {
		if got := HeaderPath(tc.input); got != tc.want {
			t.Errorf("HeaderPath(%q) = %q, want %q", tc.input, got, tc.want)
		}
	}
}
