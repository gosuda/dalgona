package patch

import (
	"strings"
	"testing"
)

func TestDecodeDocumentRejectsNULPastFirstKiB(t *testing.T) {
	raw := []byte(strings.Repeat("a", 9000) + "\x00" + "\n")
	_, err := decodeDocument("f.txt", raw)
	const want = "patch: f.txt is not a text file (binary or invalid UTF-8)"
	if err == nil || err.Error() != want {
		t.Fatalf("decodeDocument error = %v, want %q", err, want)
	}
}
