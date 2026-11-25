package agent

import (
	"context"
	"crypto/sha256"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/auth"
	"github.com/gosuda/dalgona/internal/dal/runner"
	"github.com/gosuda/dalgona/internal/dal/tool"
)

func TestExecSpillProjectionMasksOriginalJoin(t *testing.T) {
	const sensitive = "endtoken"
	raw := strings.Repeat("x", event.MaxInlineBytes) + sensitive
	w, backing, _ := newProjectionWorker(t, raw, `endtoken\n\[output truncated:`)
	text, spill := completeProjectedSpill(t, w, nil)
	notice := spillProjectionNotice(len(raw))
	want := strings.Repeat("x", event.MaxInlineBytes-len("{{secret:pattern0:0}}")) + "{{secret:pattern0:0}}" + strings.TrimPrefix(notice, "[output truncated:")
	assertProjectedSpill(t, spillProjectionCase{w: w, text: text, spill: spill, want: want, blob: raw})
	if backing.next != int64(len(raw)) {
		t.Fatalf("captured %d bytes, want %d", backing.next, len(raw))
	}
}

func TestExecSpillProjectionFindsCrossJoinBeforeTail(t *testing.T) {
	raw := "begin:" + strings.Repeat("x", spillFixturePage) + "terminal"
	w, _, _ := newProjectionWorker(t, raw, `(?s)\Abegin:.*\n\[output truncated:`)
	text, spill := completeProjectedSpill(t, w, nil)
	notice := spillProjectionNotice(len(raw))
	want := "{{secret:pattern0:0}}" + strings.TrimPrefix(notice, "[output truncated:")
	assertProjectedSpill(t, spillProjectionCase{w: w, text: text, spill: spill, want: want, blob: raw})
	restored, err := w.t.host.cfg.Auth.UnmaskStrict(text)
	if err != nil || restored != raw+"\n"+notice {
		t.Fatalf("unmasked projection is %d bytes, error %v; want complete original extent and notice", len(restored), err)
	}
}

func TestExecSpillProjectionDoesNotRescanGeneratedPlaceholder(t *testing.T) {
	const value = "secret:pattern0:0"
	const placeholderTag = "{{secret:DAL_PROJECTION_PLACEHOLDER_PASSWORD}}"
	t.Setenv("DAL_PROJECTION_PLACEHOLDER_PASSWORD", value)
	padding := strings.Repeat("q", event.MaxInlineBytes)
	raw := padding + "token-abcdef " + value
	w, _, _ := newProjectionWorker(t, raw, `token-[a-z]{6}`)
	text, spill := completeProjectedSpill(t, w, nil)
	const body = "{{secret:pattern0:0}} " + placeholderTag
	want := padding[:event.MaxInlineBytes-len(body)] + body + "\n" + spillProjectionNotice(len(raw))
	assertProjectedSpill(t, spillProjectionCase{w: w, text: text, spill: spill, want: want, blob: padding + body})
}

func TestExecSpillProjectionUsesStandaloneDiscoveriesGlobally(t *testing.T) {
	const noticePrefix = "[output truncated:"
	const eof = "raw-eof-token"
	const rawTag = "{{secret:pattern0:0}}"
	const noticeTag = "{{secret:pattern1:0}}"
	const diagnosticTag = "{{secret:pattern2:0}}"
	padding := strings.Repeat("x", event.MaxInlineBytes)
	raw := padding + noticePrefix + " " + eof
	w, _, _ := newProjectionWorker(t, raw, `raw-eof-token\z`, `\A\[output truncated:`, `offset/limit\]\ndiagnostic-start; `)
	text, spill := completeProjectedSpill(t, w, errors.New("diagnostic-start; "+eof+" detached"))
	blob := padding + noticePrefix + " " + rawTag
	tail := padding[:event.MaxInlineBytes-len(noticeTag)-1-len(rawTag)] + noticeTag + " " + rawTag
	suffix := "\n" + noticeTag + strings.TrimPrefix(strings.TrimSuffix(spillProjectionNotice(len(raw)), "offset/limit]"), noticePrefix) +
		diagnosticTag + rawTag + " detached"
	assertProjectedSpill(t, spillProjectionCase{w: w, text: text, spill: spill, want: tail + suffix, blob: blob})
}

func TestExecSpillProjectionDoesNotActivateHistoricalConfiguredMatches(t *testing.T) {
	const eofMarker = "raw-eof-token"
	padding := strings.Repeat("x", event.MaxInlineBytes)
	raw := padding + eofMarker + " detached"
	w, _, _ := newProjectionWorker(t, raw, `raw-eof-token\z`)
	store := w.t.host.cfg.Auth
	if got := store.Mask(eofMarker); got != "{{secret:pattern0:0}}" || store.MaskError() != nil {
		t.Fatalf("prior discovered token was not persisted: %v", store.MaskError())
	}
	text, spill := completeProjectedSpill(t, w, nil)
	want := raw[len(raw)-event.MaxInlineBytes:] + "\n" + spillProjectionNotice(len(raw))
	assertProjectedSpill(t, spillProjectionCase{w: w, text: text, spill: spill, want: want, blob: raw})
}

func TestExecSpillProjectionMaskFailureDoesNotPublish(t *testing.T) {
	raw := strings.Repeat("x", event.MaxInlineBytes) + "endtoken"
	w, _, home := newProjectionWorker(t, raw, `endtoken\n\[output truncated:`)
	if err := os.Mkdir(filepath.Join(home, "secrets.json"), 0o700); err != nil {
		t.Fatal(err)
	}
	p := &prepared{call: event.ToolCall{ID: "exec", Name: "exec"}, class: tool.ClassExec}
	text, spill, err := w.completionOutput(t.Context(), p, tool.Result{Path: spillFixturePath}, nil)
	if err == nil || !strings.Contains(err.Error(), "persist pattern secrets") || text != "" || spill != nil {
		t.Fatalf("projection masking failure returned %d bytes, spill %v, error %v", len(text), spill, err)
	}
	entries, err := os.ReadDir(filepath.Join(home, "blobs"))
	if err != nil || len(entries) != 0 {
		t.Fatalf("projection masking failure left %d blob entries: %v", len(entries), err)
	}
	assertNoSpillScratch(t, home)
}

func TestExecSpillProjectionLongDiagnosticHasBoundedUTF8Tail(t *testing.T) {
	raw := strings.Repeat("q", event.MaxInlineBytes+1)
	w, _, _ := newProjectionWorker(t, raw, `diagnostic-secret\z`)
	diagnostic := strings.Repeat("€", event.MaxProjectionBytes/3+4) + "diagnostic-secret"
	text, spill := completeProjectedSpill(t, w, errors.New(diagnostic))
	const tag = "{{secret:pattern0:0}}"
	want := strings.Repeat("€", (event.MaxProjectionBytes-len(tag))/3) + tag
	if !utf8.ValidString(text) {
		t.Fatal("projection split a UTF-8 rune")
	}
	assertProjectedSpill(t, spillProjectionCase{w: w, text: text, spill: spill, want: want, blob: raw})
}

type projectionCaptureRunner struct {
	pemExitRunner
	next int64
}

func (r *projectionCaptureRunner) ReadPage(ctx context.Context, id string, offset, limit int64) (runner.Output, error) {
	if offset != r.next {
		return runner.Output{}, fmt.Errorf("projection capture reread at %d, next byte is %d", offset, r.next)
	}
	page, err := r.pemExitRunner.ReadPage(ctx, id, offset, limit)
	r.next = page.Next
	return page, err
}

func newProjectionWorker(t *testing.T, raw string, patterns ...string) (*worker, *projectionCaptureRunner, string) {
	t.Helper()
	w, home := spillTestWorker(t)
	store, err := auth.NewWithPatterns(home, patterns)
	if err != nil {
		t.Fatal(err)
	}
	w.t.host.cfg.Auth = store
	backing := &projectionCaptureRunner{pemExitRunner: pemExitRunner{output: raw}}
	w.t.runner = backing
	return w, backing, home
}

func completeProjectedSpill(t *testing.T, w *worker, diagnostic error) (string, *event.Spill) {
	t.Helper()
	p := &prepared{call: event.ToolCall{ID: "exec", Name: "exec"}, class: tool.ClassExec}
	text, spill, err := w.completionOutput(t.Context(), p, tool.Result{Path: spillFixturePath}, diagnostic)
	if err != nil || spill == nil {
		t.Fatalf("formatted spill = %v, %v", spill, err)
	}
	return text, spill
}

type spillProjectionCase struct {
	w     *worker
	text  string
	spill *event.Spill
	want  string
	blob  string
}

func assertProjectedSpill(t *testing.T, c spillProjectionCase) {
	t.Helper()
	if c.text != c.want {
		t.Fatalf("projection is %d bytes, want %d independently masked bytes", len(c.text), len(c.want))
	}
	data, err := c.w.t.host.journal.Blob(c.spill.SHA256)
	digest := fmt.Sprintf("%x", sha256.Sum256([]byte(c.blob)))
	if err != nil || string(data) != c.blob || c.spill.Bytes != int64(len(c.blob)) || c.spill.SHA256 != digest || c.spill.Path != "blobs/"+digest {
		t.Fatalf("blob is %d bytes with identity %v, want %d bytes and digest %s; error %v", len(data), c.spill, len(c.blob), digest, err)
	}
}

func spillProjectionNotice(rawSize int) string {
	return fmt.Sprintf("[output truncated: %d bytes total; full output: runs/exec/out; page it with read offset/limit]", rawSize)
}
