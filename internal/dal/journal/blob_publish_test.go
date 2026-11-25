package journal

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"io"
	"os"
	"path/filepath"
	"testing"
)

func plantBlob(t *testing.T, store *Store, digest string, data []byte) {
	t.Helper()
	if err := os.WriteFile(filepath.Join(store.blobsDir(), digest), data, 0o600); err != nil {
		t.Fatal(err)
	}
}

func TestBlobStreamReplacesDamagedFile(t *testing.T) {
	store := mustOpen(t, t.TempDir())
	defer closeStore(t, store)
	good := []byte("streamed bytes that the digest names")
	sum := sha256.Sum256(good)
	digest := hex.EncodeToString(sum[:])
	plantBlob(t, store, digest, []byte("damaged bytes under the same name"))
	sha, size, err := store.PutBlobStream(t.Context(), nil, func(dst io.Writer) error {
		_, err := dst.Write(good)
		return err
	})
	if err != nil {
		t.Fatalf("PutBlobStream over a damaged file: %v", err)
	}
	if sha != digest || size != int64(len(good)) {
		t.Fatalf("PutBlobStream = %q, %d; want %q, %d", sha, size, digest, len(good))
	}
	rebuilt, total, err := store.BlobRange(digest, 0, len(good))
	if err != nil {
		t.Fatalf("BlobRange after replacement: %v", err)
	}
	if total != int64(len(good)) || !bytes.Equal(rebuilt, good) {
		t.Fatalf("BlobRange = %q of %d, want %q of %d", rebuilt, total, good, len(good))
	}
}

func TestPutBlobReplacesDamagedFile(t *testing.T) {
	store := mustOpen(t, t.TempDir())
	defer closeStore(t, store)
	good := []byte("plain bytes that the digest names")
	sum := sha256.Sum256(good)
	digest := hex.EncodeToString(sum[:])
	plantBlob(t, store, digest, []byte("damaged bytes under the same name"))
	sha, err := store.PutBlob(good)
	if err != nil {
		t.Fatalf("PutBlob over a damaged file: %v", err)
	}
	if sha != digest {
		t.Fatalf("PutBlob = %q, want %q", sha, digest)
	}
	data, err := store.Blob(digest)
	if err != nil || !bytes.Equal(data, good) {
		t.Fatalf("Blob = %q, %v; want %q", data, err, good)
	}
}

func TestBlobStreamSyncsReusedDigest(t *testing.T) {
	store := mustOpen(t, t.TempDir())
	defer closeStore(t, store)
	good := []byte("bytes stored without a directory sync")
	sum := sha256.Sum256(good)
	digest := hex.EncodeToString(sum[:])
	plantBlob(t, store, digest, good)
	synced := 0
	store.dirSync = func(string) error {
		synced++
		return nil
	}
	sha, _, err := store.PutBlobStream(t.Context(), nil, func(dst io.Writer) error {
		_, err := dst.Write(good)
		return err
	})
	if err != nil {
		t.Fatalf("PutBlobStream over a stored digest: %v", err)
	}
	if sha != digest {
		t.Fatalf("PutBlobStream = %q, want %q", sha, digest)
	}
	if synced == 0 {
		t.Fatal("reusing a stored blob reported success without a directory sync")
	}
}

func TestPutBlobReusesSealedDigestWithoutSync(t *testing.T) {
	store := mustOpen(t, t.TempDir())
	defer closeStore(t, store)
	good := []byte("bytes sealed by the first store")
	sum := sha256.Sum256(good)
	digest := hex.EncodeToString(sum[:])
	if _, err := store.PutBlob(good); err != nil {
		t.Fatalf("first PutBlob: %v", err)
	}
	store.dirSync = func(string) error {
		return errors.New("directory sync must not run for an unchanged sealed digest")
	}
	sha, err := store.PutBlob(good)
	if err != nil {
		t.Fatalf("PutBlob of a sealed digest: %v", err)
	}
	if sha != digest {
		t.Fatalf("PutBlob = %q, want %q", sha, digest)
	}
	store.dirSync = syncDir
}

func TestPutBlobReplacesChangedFile(t *testing.T) {
	store := mustOpen(t, t.TempDir())
	defer closeStore(t, store)
	good := []byte("bytes the digest names")
	sum := sha256.Sum256(good)
	digest := hex.EncodeToString(sum[:])
	if _, err := store.PutBlob(good); err != nil {
		t.Fatalf("first PutBlob: %v", err)
	}
	plantBlob(t, store, digest, []byte("changed bytes of another size"))
	sha, err := store.PutBlob(good)
	if err != nil {
		t.Fatalf("PutBlob over a changed file: %v", err)
	}
	if sha != digest {
		t.Fatalf("PutBlob = %q, want %q", sha, digest)
	}
	data, err := store.Blob(digest)
	if err != nil || !bytes.Equal(data, good) {
		t.Fatalf("Blob = %q, %v; want %q", data, err, good)
	}
}

func TestBlobStreamReplacesSameSizeDamagedFile(t *testing.T) {
	store := mustOpen(t, t.TempDir())
	defer closeStore(t, store)
	good := []byte("streamed bytes of a fixed size!!")
	sum := sha256.Sum256(good)
	digest := hex.EncodeToString(sum[:])
	damaged := bytes.Repeat([]byte("x"), len(good))
	plantBlob(t, store, digest, damaged)
	sha, size, err := store.PutBlobStream(t.Context(), nil, func(dst io.Writer) error {
		_, err := dst.Write(good)
		return err
	})
	if err != nil {
		t.Fatalf("PutBlobStream over a same-size damaged file: %v", err)
	}
	if sha != digest || size != int64(len(good)) {
		t.Fatalf("PutBlobStream = %q, %d; want %q, %d", sha, size, digest, len(good))
	}
	rebuilt, total, err := store.BlobRange(digest, 0, len(good))
	if err != nil {
		t.Fatalf("BlobRange after replacement: %v", err)
	}
	if total != int64(len(good)) || !bytes.Equal(rebuilt, good) {
		t.Fatalf("BlobRange = %q of %d, want %q of %d", rebuilt, total, good, len(good))
	}
}

func TestBlobStreamFailsUntilDirSyncSucceeds(t *testing.T) {
	store := mustOpen(t, t.TempDir())
	defer closeStore(t, store)
	good := []byte("bytes waiting on a directory sync")
	store.dirSync = func(string) error {
		return errors.New("injected dir sync failure")
	}
	if _, _, err := store.PutBlobStream(t.Context(), nil, func(dst io.Writer) error {
		_, err := dst.Write(good)
		return err
	}); err == nil {
		t.Fatal("first PutBlobStream with a failing directory sync succeeded")
	}
	if _, _, err := store.PutBlobStream(t.Context(), nil, func(dst io.Writer) error {
		_, err := dst.Write(good)
		return err
	}); err == nil {
		t.Fatal("second PutBlobStream with a failing directory sync succeeded")
	}
	synced := 0
	store.dirSync = func(string) error {
		synced++
		return nil
	}
	sum := sha256.Sum256(good)
	digest := hex.EncodeToString(sum[:])
	sha, size, err := store.PutBlobStream(t.Context(), nil, func(dst io.Writer) error {
		_, err := dst.Write(good)
		return err
	})
	if err != nil {
		t.Fatalf("PutBlobStream after directory sync recovery: %v", err)
	}
	if sha != digest || size != int64(len(good)) {
		t.Fatalf("PutBlobStream = %q, %d; want %q, %d", sha, size, digest, len(good))
	}
	if synced != 1 {
		t.Fatalf("directory sync calls after recovery = %d, want exactly one more", synced)
	}
	rebuilt, _, err := store.BlobRange(digest, 0, len(good))
	if err != nil || !bytes.Equal(rebuilt, good) {
		t.Fatalf("BlobRange = %q, %v; want %q", rebuilt, err, good)
	}
	store.dirSync = syncDir
}

func TestSecondStoreAdoptsPeerBlobOnRenameFailure(t *testing.T) {
	dir := t.TempDir()
	first := mustOpenShared(t, dir)
	defer closeStore(t, first)
	second := mustOpenShared(t, dir)
	defer closeStore(t, second)
	good := []byte("bytes both stores publish")
	sum := sha256.Sum256(good)
	digest := hex.EncodeToString(sum[:])
	second.rename = func(oldpath, newpath string) error {
		if err := os.WriteFile(newpath, good, 0o600); err != nil {
			return err
		}
		return errors.New("injected rename failure")
	}
	sha, size, err := second.PutBlobStream(t.Context(), nil, func(dst io.Writer) error {
		_, err := dst.Write(good)
		return err
	})
	if err != nil {
		t.Fatalf("second PutBlobStream with a failing rename: %v", err)
	}
	if sha != digest || size != int64(len(good)) {
		t.Fatalf("second PutBlobStream = %q, %d; want %q, %d", sha, size, digest, len(good))
	}
	assertAdoptedBlob(t, first, digest, good)
	if _, _, err := first.PutBlobStream(t.Context(), nil, func(dst io.Writer) error {
		_, err := dst.Write(good)
		return err
	}); err != nil {
		t.Fatalf("first PutBlobStream of the adopted digest: %v", err)
	}
}

func assertAdoptedBlob(t *testing.T, store *Store, digest string, want []byte) {
	t.Helper()
	data, err := store.Blob(digest)
	if err != nil || !bytes.Equal(data, want) {
		t.Fatalf("Blob = %q, %v; want %q", data, err, want)
	}
	entries, err := os.ReadDir(store.blobsDir())
	if err != nil || len(entries) != 1 || entries[0].Name() != digest {
		t.Fatalf("blobs dir holds %v, %v; want only %q", entries, err, digest)
	}
}
