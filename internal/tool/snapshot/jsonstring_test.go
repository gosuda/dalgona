package snapshot

import (
	"bytes"
	"encoding/json/jsontext"
	"testing"
)

func TestMetadataUsesJSONControlEscapes(t *testing.T) {
	data := []byte("body")
	entry := Entry{
		Ref:  "r0123456789abcdef0123456789abcdef.1",
		Path: "a\x01b", Blob: Digest(data), Revision: 1, Epoch: 1,
	}
	meta := entry.Meta()
	decoder := jsontext.NewDecoder(bytes.NewReader(meta))
	if _, err := decoder.ReadValue(); err != nil {
		t.Fatalf("metadata is not JSON: %v\n%s", err, meta)
	}
	rebuilt := New()
	if err := rebuilt.Restore(meta, data); err != nil {
		t.Fatal(err)
	}
	got, err := rebuilt.Get(entry.Ref)
	if err != nil || got.Path != entry.Path {
		t.Fatalf("round trip %+v %v", got, err)
	}
}

func TestHeaderPathUsesJSONEscapes(t *testing.T) {
	if got := HeaderPath("a\x01b"); got != `"a\u0001b"` {
		t.Fatalf("quoted path %q", got)
	}
	if got := HeaderPath("plain/f.txt"); got != "plain/f.txt" {
		t.Fatalf("plain path %q", got)
	}
}
