//go:build gui

package transcript

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

func TestTranscriptCopiesExactFenceSource(t *testing.T) {
	const code = "\tvar name = \"exact\"  \r\n\r\n\t// keep indentation\r\n"
	const source = "# Heading\r\n\r\n```go\r\n" + code + "```\r\n"
	withTranscript(t, assistantTurn(source), func(t *testing.T, h *harness) {
		h.click("Copy code")
		assertActionClipboard(t, h, code)
	})
}

func TestTranscriptDocumentActionsCopyAndSaveOriginalMarkdown(t *testing.T) {
	const source = "# Heading\r\n\r\n[plot](dalgona-blob:figure)\r\n\r\n```text\r\n  exact \t\r\n```\r\n"
	dest := filepath.Join(t.TempDir(), "source.md")
	var request platform.DialogRequest
	deps := Deps{Save: func(_ context.Context, r platform.DialogRequest) (platform.DialogResult, error) {
		request = r
		return platform.DialogResult{Files: []string{dest}}, nil
	}}
	withTranscriptDeps(t, "", deps, assistantTurn(source), func(t *testing.T, h *harness) {
		h.click("Document actions")
		h.click("Copy Markdown")
		assertActionClipboard(t, h, source)
		h.click("Save Markdown")
		h.settleUntil("Saved the Markdown to " + dest)
		if request.Kind != platform.DialogSaveFile || filepath.Ext(request.SuggestedName) != ".md" {
			t.Fatalf("Save Markdown requested %#v", request)
		}
		got, err := os.ReadFile(dest)
		if err != nil || string(got) != source {
			t.Fatalf("saved Markdown %q, err %v; want source %q", got, err, source)
		}
	})
}

func assertActionClipboard(t *testing.T, h *harness, want string) {
	t.Helper()
	mime, got, ok := h.router.WriteClipboard()
	if !ok || mime != "application/text" || string(got) != want {
		t.Fatalf("clipboard wrote (%q, %q, %t), want exact bytes %q; notice %q", mime, got, ok, want, h.tr.notice)
	}
}

func TestTranscriptMarkdownActionsNeverExportAnUnloadedPage(t *testing.T) {
	const line = "exact source, including the last two spaces.  \r\n"
	source := "# Large document\r\n\r\n" + strings.Repeat(line, 6000) + "Last line.\r\n"
	var next chan struct{}
	open := func(d *daemon) []rpc.HistoryTurn {
		next = make(chan struct{})
		turns := assistantTurn(source)(d)
		id := turns[0].Items[0].Contents[0].ContentID
		d.readGate = map[readCall]<-chan struct{}{{content: id, offset: inlineLimit}: next}
		return turns
	}
	var saveCalled bool
	deps := Deps{Save: func(context.Context, platform.DialogRequest) (platform.DialogResult, error) {
		saveCalled = true
		return platform.DialogResult{Cancelled: true}, nil
	}}
	withTranscriptDeps(t, "", deps, open, func(t *testing.T, h *harness) {
		h.click("Document actions")
		h.click("Copy Markdown")
		h.click("Save Markdown")
		if _, _, ok := h.router.WriteClipboard(); ok || saveCalled {
			t.Fatal("an incomplete document reached the clipboard or save dialog")
		}
		close(next)
		h.settle()
		h.click("Copy Markdown")
		_, got, ok := h.router.WriteClipboard()
		if !ok || string(got) != source {
			t.Fatalf("clipboard wrote %d bytes after the last page arrived, want all %d", len(got), len(source))
		}
	})
}

func TestTranscriptCloseCancelsPendingMarkdownSave(t *testing.T) {
	dest := filepath.Join(t.TempDir(), "cancelled.md")
	var started, choose chan struct{}
	deps := Deps{Save: func(context.Context, platform.DialogRequest) (platform.DialogResult, error) {
		close(started)
		<-choose
		return platform.DialogResult{Files: []string{dest}}, nil
	}}
	withTranscriptDeps(t, "", deps, assistantTurn("exact\r\n"), func(t *testing.T, h *harness) {
		started, choose = make(chan struct{}), make(chan struct{})
		h.click("Document actions")
		h.click("Save Markdown")
		select {
		case <-started:
		default:
			t.Fatal("the Save Markdown dialog did not open")
		}
		h.tr.Close()
		close(choose)
		h.wait()
		if _, err := os.Stat(dest); !errors.Is(err, os.ErrNotExist) {
			t.Fatalf("a closed window wrote the Markdown destination: %v", err)
		}
	})
}
