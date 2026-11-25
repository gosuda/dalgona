//go:build gui

package document

import (
	"context"
	"errors"
	"strings"
	"testing"

	"gioui.org/io/input"

	"github.com/gosuda/dalgona/internal/dal/textinput"
)

func clipboardText(t *testing.T, router *input.Router) (string, bool) {
	t.Helper()
	mime, content, ok := router.WriteClipboard()
	if ok && mime != "application/text" {
		t.Fatalf("clipboard type %q", mime)
	}
	return string(content), ok
}

func TestCopyMarkdownWritesTheCompleteExactSource(t *testing.T) {
	scope, pager, sources := exportScope(t)
	all, _ := scope.All()
	var router input.Router
	if err := scope.CopyMarkdown(context.Background(), router.Source(), all, pager); err != nil {
		t.Fatal(err)
	}
	if got, ok := clipboardText(t, &router); !ok || got != sources[0]+"\n"+sources[1]+"\n\n"+sources[2] {
		t.Fatalf("clipboard %q (written %v), want the exact paged Markdown source", got, ok)
	}
	pager.fail = errors.New("page 2 unavailable")
	router = input.Router{}
	if err := scope.CopyMarkdown(context.Background(), router.Source(), all, pager); !errors.Is(err, ErrPage) {
		t.Fatalf("failed page returned %v", err)
	}
	if got, ok := clipboardText(t, &router); ok {
		t.Fatalf("a failed page still copied %q", got)
	}
}

func TestCopyTextWritesOnlyCompleteAllowedText(t *testing.T) {
	scope := NewScope("local", "thread-1")
	clean := markdown(t, "clean", "first *message* text")
	nul := markdown(t, "nul", "before\x00after")
	for _, d := range []*Document{clean, nul} {
		if _, err := scope.Put(d); err != nil {
			t.Fatal(err)
		}
	}
	var router input.Router
	whole := Selection{Anchor: scope.Edge(0, false), Focus: scope.Edge(0, true)}
	if err := scope.CopyText(context.Background(), router.Source(), whole, &pages{}); err != nil {
		t.Fatal(err)
	}
	if got, ok := clipboardText(t, &router); !ok || got != "first message text" {
		t.Fatalf("clipboard %q (written %v)", got, ok)
	}
	router = input.Router{}
	withNUL := Selection{Anchor: scope.Edge(0, false), Focus: scope.Edge(1, true)}
	if err := scope.CopyText(context.Background(), router.Source(), withNUL, &pages{}); !errors.Is(err, textinput.DisallowedControl) {
		t.Fatalf("NUL copy returned %v", err)
	}
	if got, ok := clipboardText(t, &router); ok {
		t.Fatalf("a refused copy wrote %q", got)
	}
}

func TestCopyRefusesContentAboveTheClipboardLimitBeforePaging(t *testing.T) {
	scope := NewScope("local", "thread-1")
	if _, err := scope.Put(markdown(t, "small", "small")); err != nil {
		t.Fatal(err)
	}
	huge := committed(t, "huge")
	scope.entries = append(scope.entries, entry{ref: Ref{Header: Header{Identity: huge, Format: FormatCode}, Revision: 1, Length: MaxCopyBytes}})
	scope.reindex()
	pager := &pages{}
	all, _ := scope.All()
	var router input.Router
	if err := scope.CopyMarkdown(context.Background(), router.Source(), all, pager); !errors.Is(err, ErrCopyLimit) || pager.reads != 0 {
		t.Fatalf("oversized Markdown copy returned %v after %d page reads", err, pager.reads)
	}
	if err := scope.CopyText(context.Background(), router.Source(), all, pager); !errors.Is(err, ErrCopyLimit) || pager.reads != 0 {
		t.Fatalf("oversized text copy returned %v after %d page reads", err, pager.reads)
	}
	if got, ok := clipboardText(t, &router); ok {
		t.Fatalf("an oversized copy wrote %d bytes", len(got))
	}
}

func TestCopySourceWritesCodeAndPatchBytes(t *testing.T) {
	code := markdown(t, "code", "intro\n\n```go\r\na := 1\r\n\tb := 2\r\n```\r\n")
	var router input.Router
	if err := code.CopySource(router.Source(), 1); err != nil {
		t.Fatal(err)
	}
	if got, ok := clipboardText(t, &router); !ok || got != "a := 1\r\n\tb := 2\r\n" {
		t.Fatalf("Copy code wrote %q", got)
	}
	patch := patchDocument(t, "patch", strings.Join(patchSections, ""), false)
	router = input.Router{}
	if err := patch.CopySource(router.Source(), 2); err != nil {
		t.Fatal(err)
	}
	if got, ok := clipboardText(t, &router); !ok || got != patchSections[2] {
		t.Fatalf("Copy patch wrote %q, want %q", got, patchSections[2])
	}
}
