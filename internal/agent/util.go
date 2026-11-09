package agent

import (
	"crypto/rand"
	"encoding/json/jsontext"
	"fmt"
	"strconv"
	"strings"

	"github.com/gosuda/dalgona/internal/agent/event"
)

func newID(prefix string) string {
	return prefix + strings.ToLower(rand.Text()[:16])
}

func (h *Host) newThreadID() string {
	return strconv.FormatInt(h.clock().UnixMilli(), 36) + "-" + strings.ToLower(rand.Text()[:12])
}

func (h *Host) textPayload(text string) (string, *event.BlobRef, error) {
	if len(text) <= event.MaxInlineBytes {
		return text, nil, nil
	}
	sha, err := h.journal.PutBlob([]byte(text))
	if err != nil {
		return "", nil, fmt.Errorf("agent: store text blob: %w", err)
	}
	return "", &event.BlobRef{Blob: sha, Bytes: int64(len(text))}, nil
}

func (h *Host) rawPayload(value jsontext.Value) (jsontext.Value, *event.BlobRef, error) {
	if len(value) <= event.MaxInlineBytes {
		return value, nil, nil
	}
	sha, err := h.journal.PutBlob(value)
	if err != nil {
		return nil, nil, fmt.Errorf("agent: store argument blob: %w", err)
	}
	return nil, &event.BlobRef{Blob: sha, Bytes: int64(len(value))}, nil
}

func (h *Host) loadText(text string, blob *event.BlobRef) (string, error) {
	if blob == nil {
		return text, nil
	}
	data, err := h.journal.Blob(blob.Blob)
	if err != nil {
		return "", fmt.Errorf("agent: load blob %s: %w", blob.Blob, err)
	}
	return string(data), nil
}

func (h *Host) loadRaw(value jsontext.Value, blob *event.BlobRef) (jsontext.Value, error) {
	if blob == nil {
		return value, nil
	}
	data, err := h.journal.Blob(blob.Blob)
	if err != nil {
		return nil, fmt.Errorf("agent: load blob %s: %w", blob.Blob, err)
	}
	return jsontext.Value(data), nil
}
