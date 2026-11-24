//go:build gui

package transcript

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"gioui.org/io/key"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

func TestReviewExportsExactPatchAcrossPageBoundary(t *testing.T) {
	const head = `[{"path":"alpha.txt","kind":{"type":"update"},"diff":"`
	const lead = `@@ -1 +1 @@\r\n-old\r\n+`
	padding := strings.Repeat("x", (256<<10)-len(head)-len(lead)-3)
	const firstSuffix = `\uD83D\uDE00\\\"\r\n` + `","additions":1,"deletions":1},`
	const second = `{"path":"beta.txt","diff":"@@ -1 +1 @@\n-before\n+after\n","diffOmitted":false}]`
	source := head + lead + padding + firstSuffix + second
	want := "@@ -1 +1 @@\r\n-old\r\n+" + padding + "😀\\\"\r\n@@ -1 +1 @@\n-before\n+after\n"
	for _, tc := range []struct {
		name   string
		source string
		output string
		status string
	}{
		{"complete", source, want, "Exported the full text"},
		{"truncated", source[:len(source)-1], "old destination\n", "Export failed"},
		{"malformed escape", strings.Replace(source, `\uDE00`, `\uD000`, 1), "old destination\n", "Export failed"},
		{"omitted after complete file", strings.Replace(source, `"diffOmitted":false`, `"diffOmitted":true`, 1), "old destination\n", "Export failed"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			checkPatchExport(t, tc.source, tc.output, tc.status)
		})
	}
}

func checkPatchExport(t *testing.T, source, output, wantStatus string) {
	t.Helper()
	file := filepath.Join(t.TempDir(), "review.patch")
	if err := os.WriteFile(file, []byte("old destination\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	deps := Deps{Save: func(context.Context, platform.DialogRequest) (platform.DialogResult, error) {
		return platform.DialogResult{Files: []string{file}}, nil
	}}
	turn := func(d *daemon) []rpc.HistoryTurn {
		return []rpc.HistoryTurn{frozenTurn("u1", rpc.StatusCompleted,
			d.header("patch", rpc.KindFileChange, map[string]any{"status": rpc.StatusCompleted},
				fieldData{name: "changes", data: source, json: true}),
		)}
	}
	withTranscriptDeps(t, "", deps, turn, func(t *testing.T, h *harness) {
		h.keyPress("O", key.ModShortcut)
		h.frame()
		h.click("Export")
		waitReviewWork(h)
		h.tr.review.mu.Lock()
		status := h.tr.review.status
		h.tr.review.mu.Unlock()
		if !strings.HasPrefix(status, wantStatus) {
			t.Fatalf("review reported %q, want %q", status, wantStatus)
		}
		got, err := os.ReadFile(file)
		if err != nil || string(got) != output {
			t.Fatalf("exported %d bytes, want exact %d-byte patch or untouched destination: %v", len(got), len(output), err)
		}
	})
}
