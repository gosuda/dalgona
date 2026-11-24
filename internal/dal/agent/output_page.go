package agent

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
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
	if err := context.Cause(ctx); err != nil {
		return OutputPage{}, err
	}
	v, err := t.view(ctx)
	if err != nil {
		return OutputPage{}, err
	}
	spill := spilledOutput(event.ActivePath(v.records), callID)
	if spill == nil {
		return OutputPage{}, fmt.Errorf("call %s %w", callID, ErrNoSpill)
	}
	chunk, total, err := t.ReadSpillExact(ctx, spill, offset, outputPageLimit+1)
	if err != nil {
		return OutputPage{}, err
	}
	if offset == total {
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
