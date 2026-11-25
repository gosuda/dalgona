package journal

import (
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
)

var ErrBlobRange = errors.New("journal: blob range is outside the blob")

func (s *Store) BlobRange(sha string, offset int64, limit int) ([]byte, int64, error) {
	path, err := s.blobPath(sha)
	if err != nil {
		return nil, 0, err
	}
	file, err := os.Open(path)
	if errors.Is(err, fs.ErrNotExist) {
		return nil, 0, fmt.Errorf("journal: blob %s not found: %w", sha, err)
	}
	if err != nil {
		return nil, 0, err
	}
	defer file.Close()
	info, err := file.Stat()
	if err != nil {
		return nil, 0, err
	}
	total := info.Size()
	if offset < 0 || offset > total || limit < 0 {
		return nil, total, ErrBlobRange
	}
	data := make([]byte, min(int64(limit), total-offset))
	if _, err := io.ReadFull(io.NewSectionReader(file, offset, int64(len(data))), data); err != nil {
		return nil, total, fmt.Errorf("journal: read blob %s: %w", sha, err)
	}
	return data, total, nil
}
