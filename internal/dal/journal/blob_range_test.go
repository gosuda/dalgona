package journal

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"testing"
)

func TestBlobRangeReadsOnlyTheRequestedBytes(t *testing.T) {
	store := mustOpen(t, t.TempDir())
	data := bytes.Repeat([]byte("0123456789abcdef"), 4096)
	data[len(data)-1] = 'z'
	sha, err := store.PutBlob(data)
	if err != nil {
		t.Fatal(err)
	}
	rebuilt := readAllRanges(t, store, sha, len(data))
	sum := sha256.Sum256(rebuilt)
	if hex.EncodeToString(sum[:]) != sha || !bytes.Equal(rebuilt, data) {
		t.Fatal("ranges do not rebuild the blob named by its digest")
	}
	end := int64(len(data))
	tail, _, err := store.BlobRange(sha, end-1, 10000)
	if err != nil || string(tail) != "z" {
		t.Fatalf("last byte = %q, %v", tail, err)
	}
	empty, _, err := store.BlobRange(sha, end, 10000)
	if err != nil || len(empty) != 0 {
		t.Fatalf("range at the end = %d bytes, %v; want none", len(empty), err)
	}
}

func readAllRanges(t *testing.T, store *Store, sha string, size int) []byte {
	t.Helper()
	var rebuilt []byte
	for offset := int64(0); offset < int64(size); {
		page, total, err := store.BlobRange(sha, offset, 10000)
		if err != nil {
			t.Fatalf("range at %d: %v", offset, err)
		}
		if total != int64(size) || len(page) > 10000 || len(page) == 0 {
			t.Fatalf("range at %d = %d bytes of %d, want at most 10000 of %d", offset, len(page), total, size)
		}
		rebuilt = append(rebuilt, page...)
		offset += int64(len(page))
	}
	return rebuilt
}

func TestBlobRangeRefusesBadRequests(t *testing.T) {
	store := mustOpen(t, t.TempDir())
	sha, err := store.PutBlob([]byte("small blob"))
	if err != nil {
		t.Fatal(err)
	}
	for _, tc := range []struct {
		offset int64
		limit  int
	}{{11, 1}, {-1, 1}, {0, -1}} {
		if _, _, err := store.BlobRange(sha, tc.offset, tc.limit); !errors.Is(err, ErrBlobRange) {
			t.Fatalf("range %d+%d error = %v, want ErrBlobRange", tc.offset, tc.limit, err)
		}
	}
	if _, _, err := store.BlobRange("../"+sha[3:], 0, 1); err == nil {
		t.Fatal("a malformed blob id was read")
	}
	missing := sha256.Sum256([]byte("never stored"))
	if _, _, err := store.BlobRange(hex.EncodeToString(missing[:]), 0, 1); err == nil {
		t.Fatal("a missing blob was read")
	}
}
