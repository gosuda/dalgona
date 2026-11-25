package journal

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"hash"
	"io"
	"io/fs"
	"os"
	"path/filepath"
)

type BlobIdentity struct {
	SHA256 string
	Bytes  int64
}

var ErrBlobIdentity = errors.New("journal: blob content does not match its recorded identity")

type blobFingerprint struct {
	size  int64
	mtime int64
}

func (s *Store) PutBlobStream(ctx context.Context, expected *BlobIdentity, produce func(io.Writer) error) (sha string, size int64, retErr error) {
	if err := context.Cause(ctx); err != nil {
		return "", 0, err
	}
	dir := s.blobsDir()
	stage, err := os.CreateTemp(dir, ".tmp-*")
	if err != nil {
		return "", 0, err
	}
	defer func() { retErr = errors.Join(retErr, removeBlobStage(stage.Name())) }()
	stream := blobStream{ctx: ctx, stage: stage, hash: sha256.New()}
	if err := writeBlobStream(&stream, produce); err != nil {
		return "", 0, err
	}
	return s.publishBlob(&stream, expected, dir)
}

func (s *Store) publishBlob(stream *blobStream, expected *BlobIdentity, dir string) (string, int64, error) {
	if err := context.Cause(stream.ctx); err != nil {
		return "", 0, err
	}
	sha := hex.EncodeToString(stream.hash.Sum(nil))
	if expected != nil && (expected.SHA256 != sha || expected.Bytes != stream.size) {
		return "", 0, ErrBlobIdentity
	}
	dest := filepath.Join(dir, sha)
	s.blobMu.Lock()
	defer s.blobMu.Unlock()
	reused, err := s.reuseBlob(dest, sha, stream.size)
	if err != nil {
		return "", 0, err
	}
	if reused {
		return sha, stream.size, nil
	}
	if err := s.renameBlobStage(stream.stage.Name(), dest, sha); err != nil {
		return "", 0, err
	}
	return sha, stream.size, nil
}

func (s *Store) reuseBlob(dest, sha string, size int64) (bool, error) {
	info, err := os.Stat(dest)
	if errors.Is(err, fs.ErrNotExist) {
		return false, nil
	}
	if err != nil {
		return false, err
	}
	if info.IsDir() || info.Size() != size {
		return false, nil
	}
	if s.blobSealed(sha, info) {
		return true, nil
	}
	match, err := hashBlobFile(dest, sha)
	if err != nil || !match {
		return false, err
	}
	if err := s.syncSealedBlob(dest, sha); err != nil {
		return false, err
	}
	return true, nil
}

func (s *Store) blobSealed(sha string, info os.FileInfo) bool {
	return s.syncedBlobs[sha] == blobFingerprintOf(info)
}

func blobFingerprintOf(info os.FileInfo) blobFingerprint {
	return blobFingerprint{size: info.Size(), mtime: info.ModTime().UnixNano()}
}

func verifyBlobBytes(dest, want string) (bool, error) {
	info, err := os.Stat(dest)
	if errors.Is(err, fs.ErrNotExist) {
		return false, nil
	}
	if err != nil {
		return false, err
	}
	if info.IsDir() {
		return false, nil
	}
	return hashBlobFile(dest, want)
}

func hashBlobFile(dest, want string) (bool, error) {
	file, err := os.Open(dest)
	if errors.Is(err, fs.ErrNotExist) {
		return false, nil
	}
	if err != nil {
		return false, err
	}
	defer file.Close()
	sum := sha256.New()
	if _, err := io.Copy(sum, file); err != nil {
		return false, err
	}
	return hex.EncodeToString(sum.Sum(nil)) == want, nil
}

func (s *Store) renameBlobStage(stage, dest, sha string) error {
	if err := s.rename(stage, dest); err != nil {
		verified, verr := verifyBlobBytes(dest, sha)
		if verr != nil {
			return errors.Join(err, verr)
		}
		if !verified {
			return s.replaceDamagedBlob(stage, dest, sha, err)
		}
		if rerr := removeBlobStage(stage); rerr != nil {
			return rerr
		}
		return s.syncSealedBlob(dest, sha)
	}
	return s.syncSealedBlob(dest, sha)
}

func (s *Store) syncSealedBlob(dest, sha string) error {
	if err := s.dirSync(filepath.Dir(dest)); err != nil {
		return err
	}
	info, err := os.Stat(dest)
	if err == nil && !info.IsDir() {
		s.syncedBlobs[sha] = blobFingerprintOf(info)
	}
	return nil
}

func (s *Store) replaceDamagedBlob(stage, dest, sha string, renameErr error) error {
	if rerr := removeDamagedBlob(dest); rerr != nil {
		return errors.Join(renameErr, rerr)
	}
	if rerr := s.rename(stage, dest); rerr != nil {
		return errors.Join(renameErr, rerr)
	}
	return s.syncSealedBlob(dest, sha)
}

func removeDamagedBlob(dest string) error {
	info, err := os.Stat(dest)
	if errors.Is(err, fs.ErrNotExist) {
		return nil
	}
	if err != nil {
		return err
	}
	if info.IsDir() {
		return nil
	}
	if err := os.Remove(dest); err != nil && !errors.Is(err, fs.ErrNotExist) {
		return err
	}
	return nil
}

func removeBlobStage(path string) error {
	err := os.Remove(path)
	if errors.Is(err, fs.ErrNotExist) {
		return nil
	}
	return err
}

type blobStream struct {
	ctx   context.Context
	stage *os.File
	hash  hash.Hash
	size  int64
}

func writeBlobStream(stream *blobStream, produce func(io.Writer) error) (retErr error) {
	defer func() { retErr = errors.Join(retErr, stream.stage.Close()) }()
	if err := produce(stream); err != nil {
		return err
	}
	if err := context.Cause(stream.ctx); err != nil {
		return err
	}
	return stream.stage.Sync()
}

func (s *blobStream) Write(data []byte) (int, error) {
	if err := context.Cause(s.ctx); err != nil {
		return 0, err
	}
	n, err := s.stage.Write(data)
	_, _ = s.hash.Write(data[:n])
	s.size += int64(n)
	return n, err
}
