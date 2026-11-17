package agent

import (
	"os"
	"path/filepath"
	"runtime"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/config"
)

func spillCommand(t *testing.T) string {
	t.Helper()
	if runtime.GOOS == "windows" {
		exe, err := os.Executable()
		if err != nil {
			t.Fatal(err)
		}
		t.Setenv("PATH", filepath.Dir(exe)+string(os.PathListSeparator)+os.Getenv("PATH"))
		return filepath.Base(exe) + " __spill"
	}
	return `head -c 1100000 /dev/zero | LC_ALL=C tr '\\0' '\\200'`
}

func TestBinarySpillPagesAlwaysAdvance(t *testing.T) {
	const size = 1100000
	h := newHarness(t, withApproval(config.ApprovalYolo))
	h.model.push(
		calls(call{id: "call-1", name: "exec", args: `{"command":"` + spillCommand(t) + `"}`}),
		text("done"),
	)
	thread := h.start()
	h.submit(thread, "print binary")
	h.waitIdle(thread)
	var offset int64
	for pages := 1; ; pages++ {
		if pages > 4 {
			t.Fatalf("paging did not end after %d pages at offset %d", pages-1, offset)
		}
		page, err := thread.ReadOutput(t.Context(), "call-1", offset)
		if err != nil {
			t.Fatal(err)
		}
		if page.Total != size {
			t.Fatalf("total = %d, want %d", page.Total, size)
		}
		if page.Next <= offset {
			t.Fatalf("page at offset %d made no progress; next = %d", offset, page.Next)
		}
		offset = page.Next
		if offset == page.Total {
			return
		}
	}
}
