package figure

import (
	"strings"
	"testing"
)

func TestRenderRefusesWorkWhenTheQueueIsSaturated(t *testing.T) {
	sup := &Supervisor{}
	for range PendingJobLimit {
		sup.queue = append(sup.queue, &job{})
	}
	_, _, err := sup.Render(Request{Kind: KindMermaid, Source: []byte(strings.Repeat("graph TD\nA-->B\n", 8))})
	if err == nil || !strings.Contains(err.Error(), "waiting") {
		t.Fatalf("saturated render returned %v, want the waiting signal", err)
	}
}
