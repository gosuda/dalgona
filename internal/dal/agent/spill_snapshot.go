package agent

import (
	"context"
	"crypto/aes"
	"crypto/cipher"
	"crypto/rand"
	"encoding/binary"
	"errors"
	"io"
	"os"

	"github.com/gosuda/dalgona/internal/dal/runner"
)

type spillSnapshot struct {
	ctx    context.Context
	file   *os.File
	aead   cipher.AEAD
	size   int64
	base   int64
	sealed []byte
	plain  []byte
}

func newSpillSnapshot(ctx context.Context, size int64) (*spillSnapshot, error) {
	var key [32]byte
	if _, err := rand.Read(key[:]); err != nil {
		return nil, err
	}
	block, err := aes.NewCipher(key[:])
	clear(key[:])
	if err != nil {
		return nil, err
	}
	aead, err := cipher.NewGCMWithRandomNonce(block)
	if err != nil {
		return nil, err
	}
	file, err := os.CreateTemp("", "dalgona-exec-*")
	if err != nil {
		return nil, err
	}
	return &spillSnapshot{
		ctx: ctx, file: file, aead: aead, size: size, base: -1,
		sealed: make([]byte, 0, runner.MaxProcessPageBytes+aead.Overhead()),
		plain:  make([]byte, 0, runner.MaxProcessPageBytes),
	}, nil
}

func (s *spillSnapshot) close() error {
	clear(s.plain[:cap(s.plain)])
	return errors.Join(s.file.Close(), os.Remove(s.file.Name()))
}

func (s *spillSnapshot) append(offset int64, page []byte) error {
	var record [binary.MaxVarintLen64]byte
	binary.PutVarint(record[:], offset)
	sealed := s.aead.Seal(s.sealed[:0], nil, page, record[:]) //nolint:gosec // G407: the AEAD generates a random nonce and requires an empty argument
	_, err := s.file.Write(sealed)
	return err
}

func (s *spillSnapshot) ReadAt(dst []byte, offset int64) (int, error) {
	if offset < 0 {
		return 0, errors.New("agent: negative spill snapshot offset")
	}
	read := 0
	for read < len(dst) && offset < s.size {
		if err := s.load(offset); err != nil {
			return read, err
		}
		n := copy(dst[read:], s.plain[offset-s.base:])
		read += n
		offset += int64(n)
	}
	if read < len(dst) {
		return read, io.EOF
	}
	return read, nil
}

func (s *spillSnapshot) load(offset int64) error {
	if err := context.Cause(s.ctx); err != nil {
		return err
	}
	page := offset / runner.MaxProcessPageBytes
	base := page * runner.MaxProcessPageBytes
	if s.base == base {
		return nil
	}
	length := min(runner.MaxProcessPageBytes, s.size-base)
	sealed := s.sealed[:length+int64(s.aead.Overhead())]
	n, err := s.file.ReadAt(sealed, base+page*int64(s.aead.Overhead()))
	if err != nil && !errors.Is(err, io.EOF) {
		return err
	}
	if n != len(sealed) {
		return io.ErrUnexpectedEOF
	}
	var record [binary.MaxVarintLen64]byte
	binary.PutVarint(record[:], base)
	plain, err := s.aead.Open(s.plain[:0], nil, sealed, record[:])
	if err != nil {
		return err
	}
	s.base, s.plain = base, plain
	return nil
}
