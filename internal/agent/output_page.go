package agent

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"net/url"
	"strings"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/agent/event"
)

const outputPageLimit = 1 << 20

var ErrNoSpill = errors.New("has no spilled output")

type OffsetError struct {
	Offset int64
	Total  int64
}

func (e OffsetError) Error() string {
	return fmt.Sprintf("offset %d is outside 0..%d", e.Offset, e.Total)
}

type OutputPage struct {
	Text   string
	Offset int64
	Next   int64
	Total  int64
}

func (t *Thread) ReadOutput(ctx context.Context, callID string, offset int64) (OutputPage, error) {
	records, _, err := t.Records()
	if err != nil {
		return OutputPage{}, err
	}
	spill := spilledOutput(event.ActivePath(records), callID)
	if spill == nil {
		return OutputPage{}, fmt.Errorf("call %s %w", callID, ErrNoSpill)
	}
	chunk, total, err := t.spillFrom(ctx, callID, spill, offset)
	if err != nil {
		return OutputPage{}, err
	}
	if offset < 0 || offset >= total {
		return OutputPage{}, OffsetError{Offset: offset, Total: total}
	}
	kept := chunk[:pageEnd(chunk)]
	text, err := t.host.mask(string(kept))
	if err != nil {
		return OutputPage{}, err
	}
	return OutputPage{Text: text, Offset: offset, Next: offset + int64(len(kept)), Total: total}, nil
}

func spilledOutput(path []event.Seqd, callID string) *event.Spill {
	var found *event.Spill
	for _, rec := range path {
		if done, ok := rec.Event.(event.ToolCompleted); ok && done.CallID == callID && done.Spill != nil {
			found = done.Spill
		}
	}
	return found
}

func (t *Thread) spillFrom(ctx context.Context, callID string, spill *event.Spill, offset int64) ([]byte, int64, error) {
	if sha, ok := strings.CutPrefix(spill.Path, "blobs/"); ok {
		return t.blobSpillFrom(sha, spill, offset)
	}
	return t.runnerSpillFrom(ctx, callID, spill, offset)
}

func (t *Thread) blobSpillFrom(sha string, spill *event.Spill, offset int64) ([]byte, int64, error) {
	blob, err := t.host.Blob(&event.BlobRef{Blob: sha, Bytes: spill.Bytes})
	if err != nil {
		return nil, 0, err
	}
	if offset < 0 || offset >= int64(len(blob)) {
		return nil, spill.Bytes, nil
	}
	return blob[offset:], spill.Bytes, nil
}

func (t *Thread) runnerSpillFrom(ctx context.Context, callID string, spill *event.Spill, offset int64) ([]byte, int64, error) {
	info, err := t.runner.Stat(ctx, spill.Path)
	if err != nil {
		return nil, 0, fmt.Errorf("agent: stat output %s: %w", spill.Path, err)
	}
	if offset < 0 || offset >= info.Size {
		return nil, info.Size, nil
	}
	page, err := t.runner.Read(ctx, url.PathEscape(callID), offset)
	if err != nil {
		return nil, 0, fmt.Errorf("agent: read output %s: %w", spill.Path, err)
	}
	if len(page.Data) == 0 {
		return nil, 0, fmt.Errorf("agent: read output %s: no data at offset %d of %d", spill.Path, offset, info.Size)
	}
	return page.Data, info.Size, nil
}

func pageEnd(chunk []byte) int {
	if len(chunk) <= outputPageLimit {
		return len(chunk)
	}
	if i := bytes.LastIndexByte(chunk[:outputPageLimit], '\n'); i >= 0 {
		return i + 1
	}
	for end := outputPageLimit; end > 0; end-- {
		if utf8.RuneStart(chunk[end]) {
			return end
		}
	}
	return outputPageLimit
}
