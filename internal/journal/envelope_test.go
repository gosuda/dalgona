package journal

import (
	"bytes"
	"context"
	"encoding/hex"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"hash/crc32"
	"io"
	"strconv"
	"strings"
	"testing"
)

func TestEnvelopeIsJSON(t *testing.T) {
	dir := t.TempDir()
	store := mustOpen(t, dir)
	written := [][]byte{
		[]byte(`{"type":"text","body":"quote \" slash \\ tab \t"}`),
		[]byte(`{"type":"nested","items":[1,{"k":null}],"flag":true}`),
		[]byte(`{"type":"unicode","s":"\u00e9 \ud83d\ude00"}`),
	}
	if _, err := store.Append(context.Background(), "s", written...); err != nil {
		t.Fatal(err)
	}
	closeStore(t, store)
	lines := bytes.Split(bytes.TrimSuffix(readFile(t, journalFile(dir)), []byte("\n")), []byte("\n"))
	if len(lines) != len(written) {
		t.Fatalf("line count = %d, want %d", len(lines), len(written))
	}
	table := crc32.MakeTable(crc32.Castagnoli)
	for i, line := range lines {
		fields := decodeTopLevel(t, line)
		ev := fields["ev"]
		if ev.Kind() != jsontext.KindBeginObject || !bytes.Equal(ev, written[i]) {
			t.Fatalf("line %d ev = %s", i, ev)
		}
		crcText := unquoteHex(t, fields["crc"], 8)
		if want := fmt.Sprintf("%08x", crc32.Checksum(ev, table)); crcText != want {
			t.Fatalf("line %d crc = %s, want %s", i, crcText, want)
		}
		unquoteHex(t, fields["prev"], 64)
		unquoteHex(t, fields["hash"], 64)
		if seq := string(fields["seq"]); seq != strconv.Itoa(i+1) {
			t.Fatalf("line %d seq = %s", i, seq)
		}
	}
}

func decodeTopLevel(t *testing.T, line []byte) map[string]jsontext.Value {
	t.Helper()
	dec := jsontext.NewDecoder(bytes.NewReader(line))
	if tok, err := dec.ReadToken(); err != nil || tok.Kind() != jsontext.KindBeginObject {
		t.Fatalf("line %s: open object: %v", line, err)
	}
	fields := map[string]jsontext.Value{}
	for dec.PeekKind() != jsontext.KindEndObject {
		token, err := dec.ReadToken()
		if err != nil {
			t.Fatalf("line %s: name: %v", line, err)
		}
		name := token.String()
		value, err := dec.ReadValue()
		if err != nil {
			t.Fatalf("line %s: value: %v", line, err)
		}
		fields[name] = bytes.Clone(value)
	}
	if _, err := dec.ReadToken(); err != nil {
		t.Fatalf("line %s: close object: %v", line, err)
	}
	if _, err := dec.ReadToken(); !errors.Is(err, io.EOF) {
		t.Fatalf("line %s: trailing data: %v", line, err)
	}
	return fields
}

func unquoteHex(t *testing.T, value jsontext.Value, size int) string {
	t.Helper()
	if value.Kind() != '"' || len(value) != size+2 {
		t.Fatalf("field %s is not a %d-character string", value, size)
	}
	text := string(value[1 : len(value)-1])
	if _, err := hex.DecodeString(text); err != nil || strings.ToLower(text) != text {
		t.Fatalf("field %s is not lowercase hex", value)
	}
	return text
}
