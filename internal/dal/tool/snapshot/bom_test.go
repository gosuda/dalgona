package snapshot

import (
	"bytes"
	"testing"
)

func TestTrimBOMSplitsOnlyTheLeadingMark(t *testing.T) {
	const bom = "\ufeff"
	tests := []struct {
		name string
		data string
		text string
		mark bool
	}{
		{"empty", "", "", false},
		{"BOM only", bom, "", true},
		{"BOM and CRLF", bom + "\r\n", "\r\n", true},
		{"BOM and a line", bom + "one\n", "one\n", true},
		{"a second BOM stays content", bom + bom, bom, true},
		{"a BOM after the start stays content", "a" + bom, "a" + bom, false},
		{"two bytes of the mark", "\xef\xbb", "\xef\xbb", false},
		{"no mark", "one\n", "one\n", false},
	}
	for _, test := range tests {
		t.Run("GAP-01/"+test.name, func(t *testing.T) {
			text, mark := TrimBOM([]byte(test.data))
			if string(text) != test.text || mark != test.mark {
				t.Fatalf("TrimBOM(%q) = %q, %v; want %q, %v", test.data, text, mark, test.text, test.mark)
			}
		})
	}
}

func TestLinesIgnoreALeadingBOMAndKeepTheStoredBytes(t *testing.T) {
	const bom = "\ufeff"
	tests := []struct {
		name   string
		data   string
		lines  int
		digest string
	}{
		{"BOM only", bom, 0, "f1945cd6c19e56b3c1c78943ef5ec18116907a4ca1efc40a57d48ab1db7adfc5"},
		{"BOM and an empty line", bom + "\n", 1, ""},
		{"BOM and CRLF", bom + "\r\n", 1, ""},
		{"BOM and a CRLF line", bom + "one\r\n", 1, "e37265189a9c48ed89240b3c30a5cd4eabf813e0a778fce7a3bdf8b2dfe961c6"},
		{"BOM and one unterminated line", bom + "one", 1, ""},
		{"BOM and two lines", bom + "one\ntwo\n", 2, ""},
		{"BOM and an unterminated second line", bom + "one\ntwo", 2, ""},
		{"a second BOM is content", bom + bom, 1, ""},
		{"a BOM after the start is content", "a" + bom, 1, ""},
	}
	store := New()
	for _, test := range tests {
		t.Run("GAP-01 GAP-04/"+test.name, func(t *testing.T) {
			ref := store.Put(test.name+".txt", []byte(test.data), []Range{{Start: 1, End: 1}})
			got, err := store.Lines(ref)
			if err != nil || got != test.lines {
				t.Fatalf("Lines(%q) = %d, %v; want %d", test.data, got, err, test.lines)
			}
			requireStoredBytes(t, store, ref, test.data, test.digest)
			if entry, _ := store.Get(ref); test.lines == 0 && len(entry.Shown) != 0 {
				t.Fatalf("a document with no lines keeps shown rows %v", entry.Shown)
			}
		})
	}
}

func requireStoredBytes(t *testing.T, store *Store, ref, data, digest string) {
	t.Helper()
	entry, err := store.Get(ref)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(entry.Data, []byte(data)) {
		t.Fatalf("stored bytes %q, want %q", entry.Data, data)
	}
	if digest != "" && entry.Blob != digest {
		t.Fatalf("stored digest %s, want %s", entry.Blob, digest)
	}
}
