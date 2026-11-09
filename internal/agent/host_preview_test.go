package agent

import (
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
)

func TestResolvePreviewPrefersEarliestBlob(t *testing.T) {
	h := newHarness(t).host
	sha, err := h.journal.PutBlob([]byte("blobbed first prompt"))
	if err != nil {
		t.Fatalf("store blob: %v", err)
	}
	stored := event.State{
		Preview:     "second",
		PreviewBlob: &event.BlobRef{Blob: sha, Bytes: 20},
	}
	if got := h.resolvePreview("thread", stored); got != "blobbed first prompt" {
		t.Fatalf("preview = %q, want the earlier blobbed prompt", got)
	}
	dangling := event.State{
		Preview:     "second",
		PreviewBlob: &event.BlobRef{Blob: "0000000000000000000000000000000000000000000000000000000000000000", Bytes: 8},
	}
	if got := h.resolvePreview("thread", dangling); got != "second" {
		t.Fatalf("preview = %q, want the later prompt when the blob cannot load", got)
	}
}
