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
	return stream.publish(expected, dir)
}

func (s *blobStream) publish(expected *BlobIdentity, dir string) (string, int64, error) {
	if err := context.Cause(s.ctx); err != nil {
		return "", 0, err
	}
	sha := hex.EncodeToString(s.hash.Sum(nil))
	if expected != nil && (expected.SHA256 != sha || expected.Bytes != s.size) {
		return "", 0, ErrBlobIdentity
	}
	if err := os.Rename(s.stage.Name(), filepath.Join(dir, sha)); err != nil {
		return "", 0, err
	}
	if err := syncDir(dir); err != nil {
		return "", 0, err
	}
	return sha, s.size, nil
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
