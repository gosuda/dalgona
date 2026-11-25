package agent

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"strings"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/journal"
)

var ErrSpillChanged = errors.New("agent: spilled output changed since it was recorded")

func (t *Thread) ReadSpillExact(ctx context.Context, spill *event.Spill, offset, limit int64) ([]byte, int64, error) {
	if spill == nil {
		return nil, 0, ErrNoSpill
	}
	sha, blob := strings.CutPrefix(spill.Path, "blobs/")
	if blob && sha != spill.SHA256 {
		return nil, 0, fmt.Errorf("agent: spill %s digest %s: %w", spill.Path, spill.SHA256, ErrSpillChanged)
	}
	if err := validateSpillWindow(ctx, spill.Bytes, offset, limit); err != nil {
		return nil, spill.Bytes, err
	}
	if offset == spill.Bytes {
		return []byte{}, spill.Bytes, nil
	}
	count := min(limit, spill.Bytes-offset)
	data, total, err := t.verifiedSpillWindow(spill, offset, count)
	if !blob && errors.Is(err, fs.ErrNotExist) {
		return t.admitLegacySpillWindow(ctx, spill, offset, count)
	}
	return data, total, err
}

func validateSpillWindow(ctx context.Context, total, offset, limit int64) error {
	if offset < 0 || offset > total || limit < 0 {
		return OffsetError{Offset: offset, Total: total}
	}
	return context.Cause(ctx)
}

func (t *Thread) admitLegacySpillWindow(ctx context.Context, spill *event.Spill, offset, limit int64) ([]byte, int64, error) {
	if err := t.materializeSpill(ctx, spill); err != nil {
		return nil, 0, err
	}
	return t.verifiedSpillWindow(spill, offset, limit)
}

func (t *Thread) verifiedSpillWindow(spill *event.Spill, offset, limit int64) ([]byte, int64, error) {
	data, total, err := t.host.journal.BlobRange(spill.SHA256, offset, int(limit))
	if err != nil && !errors.Is(err, journal.ErrBlobRange) {
		return nil, 0, err
	}
	if total != spill.Bytes || int64(len(data)) != limit || err != nil {
		return nil, 0, fmt.Errorf("agent: output %s length changed: %w", spill.Path, ErrSpillChanged)
	}
	if int64(len(data)) == total {
		sum := sha256.Sum256(data)
		if hex.EncodeToString(sum[:]) != spill.SHA256 {
			return nil, 0, fmt.Errorf("agent: output %s content changed: %w", spill.Path, ErrSpillChanged)
		}
	}
	return data, total, nil
}

type spillFilePager interface {
	ReadFilePage(context.Context, string, int64, int64) ([]byte, int64, error)
}

func (t *Thread) materializeSpill(ctx context.Context, spill *event.Spill) error {
	pager, ok := t.runner.(spillFilePager)
	if !ok {
		return errors.New("agent: runner does not support bounded spill file reads")
	}
	info, err := t.runner.Stat(ctx, spill.Path)
	if err != nil {
		return fmt.Errorf("agent: stat output %s: %w", spill.Path, err)
	}
	source := spillPages{size: info.Size, read: func(ctx context.Context, offset, limit int64) ([]byte, int64, error) {
		return pager.ReadFilePage(ctx, spill.Path, offset, limit)
	}}
	expected := journal.BlobIdentity{SHA256: spill.SHA256, Bytes: spill.Bytes}
	_, _, err = t.host.journal.PutBlobStream(ctx, &expected, func(dst io.Writer) error {
		return t.captureLegacySpill(ctx, spill.Path, source, dst)
	})
	if errors.Is(err, journal.ErrBlobIdentity) || errors.Is(err, io.ErrUnexpectedEOF) {
		return fmt.Errorf("agent: output %s content changed: %w", spill.Path, ErrSpillChanged)
	}
	return err
}

func (t *Thread) captureLegacySpill(ctx context.Context, path string, source spillPages, dst io.Writer) error {
	if err := writeSpill(ctx, t.host.cfg.Auth, source, dst); err != nil {
		return err
	}
	info, err := t.runner.Stat(ctx, path)
	if err != nil {
		return fmt.Errorf("agent: stat output %s: %w", path, err)
	}
	if info.Size != source.size {
		return fmt.Errorf("agent: output %s length changed: %w", path, ErrSpillChanged)
	}
	return nil
}
