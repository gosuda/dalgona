//go:build gui

package transcript

import (
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dalgona/gui/access/accesstest"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

func TestAccessibilityTranscriptReadsHeadingsLinksAndCode(t *testing.T) {
	const url = "https://example.org/guide"
	const text = "## Unicode 😀\n\nRead the [guide](" + url + ").\n\n```go\nfmt.Println(42)\n```"
	var opened []string
	deps := Deps{OpenLink: func(s string) error { opened = append(opened, s); return nil }}
	withTranscriptDeps(t, "", deps, assistantTurn(text), func(t *testing.T, h *harness) {
		h.ax = accesstest.New()
		h.frame()
		head := h.ax.Find(t, "Unicode 😀", platform.RoleHeading)
		if head.Level != 2 || len(head.Lines) != 1 || head.Lines[0].Bounds.Intersect(head.Bounds).Empty() {
			t.Fatalf("heading lacks level or shaped line geometry: %+v", head)
		}
		code := h.ax.Find(t, "Code", platform.RoleCode)
		if code.Description != "go" || !strings.Contains(code.Value, "fmt.Println(42)") {
			t.Fatalf("code lacks language or readable text: %+v", code)
		}
		link := h.ax.Find(t, "guide", platform.RoleLink)
		h.ax.Request(t, link, platform.ActionActivate)
		h.frame()
		h.frame()
		if len(opened) != 1 || opened[0] != url {
			t.Fatalf("open link received %v", opened)
		}
	})
}

func TestAccessibilityTranscriptRetainsScrolledOffIdentity(t *testing.T) {
	text := "## First row\n\n" + strings.Repeat("another paragraph\n\n", 80)
	withTranscript(t, assistantTurn(text), func(t *testing.T, h *harness) {
		h.ax = accesstest.New()
		h.frame()
		offscreen := h.ax.Find(t, "First row", platform.RoleHeading)
		if !offscreen.Bounds.Empty() {
			t.Fatalf("offscreen heading has bounds %v", offscreen.Bounds)
		}
		h.ax.Request(t, offscreen, platform.ActionScrollIntoView)
		h.frame()
		visible := h.ax.Find(t, "First row", platform.RoleHeading)
		if visible.ID != offscreen.ID || visible.Bounds.Empty() {
			t.Fatalf("scroll lost the heading or failed to reveal it: offscreen %+v, visible %+v", offscreen, visible)
		}
	})
}
