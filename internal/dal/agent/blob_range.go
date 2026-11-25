package agent

import (
	"errors"
	"fmt"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
)

func (h *Host) BlobRange(ref *event.BlobRef, offset int64, limit int) ([]byte, int64, error) {
	if ref == nil {
		return nil, 0, errors.New("agent: nil blob reference")
	}
	data, total, err := h.journal.BlobRange(ref.Blob, offset, limit)
	if err != nil {
		return nil, total, fmt.Errorf("agent: blob %s: %w", ref.Blob, err)
	}
	return data, total, nil
}
