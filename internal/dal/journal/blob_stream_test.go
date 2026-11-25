package journal

import (
	"crypto/sha256"
	"encoding/hex"
	"io"
	"os"
	"path/filepath"
	"testing"
)

func blockBlobDigest(t *testing.T, store *Store, digest string) {
	t.Helper()
	blocked := filepath.Join(store.blobsDir(), digest)
	if err := os.Mkdir(blocked, 0o700); err != nil {
		t.Fatal(err)
	}
}

func requireFailedPublish(t *testing.T, staged bool, sha string, size int64, err error) {
	t.Helper()
	if !staged || err == nil || sha != "" || size != 0 {
		t.Fatalf("failed rename gave written=%t, sha=%q, size=%d, err=%v", staged, sha, size, err)
	}
}

func requireOnlyBlockRemains(t *testing.T, store *Store, digest string) {
	t.Helper()
	entries, err := os.ReadDir(store.blobsDir())
	if err != nil || len(entries) != 1 || entries[0].Name() != digest || !entries[0].IsDir() {
		t.Fatalf("failed publication left entries %v, error %v", entries, err)
	}
}

func TestBlobStreamFailedPublishRemovesStaging(t *testing.T) {
	store := mustOpen(t, t.TempDir())
	const text = "a finished masked stream"
	sum := sha256.Sum256([]byte(text))
	digest := hex.EncodeToString(sum[:])
	blockBlobDigest(t, store, digest)
	var staged bool
	sha, size, err := store.PutBlobStream(t.Context(), nil, func(dst io.Writer) error {
		_, err := io.WriteString(dst, text)
		staged = err == nil
		return err
	})
	requireFailedPublish(t, staged, sha, size, err)
	requireOnlyBlockRemains(t, store, digest)
}
