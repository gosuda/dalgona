//go:build gui

package transcript

import (
	"context"
	"errors"
	"io"
	"sync"

	"github.com/gosuda/dalgona/internal/dalgona/gui/document"
	"github.com/gosuda/dalgona/internal/dalgona/gui/figure"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

var errNoFaces = errors.New("figure export needs the bundled text faces; restart dalgona")

type saver func(context.Context, platform.DialogRequest) (platform.DialogResult, error)

type exporter struct {
	mu     sync.Mutex
	busy   bool
	status string
	done   bool
	stop   context.CancelFunc
}

func (e *exporter) claim() (context.Context, bool) {
	e.mu.Lock()
	defer e.mu.Unlock()
	if e.busy {
		return nil, false
	}
	ctx, cancel := context.WithCancel(context.Background())
	e.busy, e.stop = true, cancel
	return ctx, true
}

func (e *exporter) finish(status string) {
	e.mu.Lock()
	e.stop()
	e.stop = nil
	e.busy, e.status, e.done = false, status, true
	e.mu.Unlock()
}

func (e *exporter) cancel() {
	e.mu.Lock()
	stop := e.stop
	e.mu.Unlock()
	if stop != nil {
		stop()
	}
}

func (e *exporter) take() (string, bool) {
	e.mu.Lock()
	defer e.mu.Unlock()
	status, done := e.status, e.done
	e.done = false
	return status, done
}

func (t *Transcript) startSave(title, name string, write func(io.Writer) error) {
	t.beginSave(saveRequest{title: title, filename: name, write: write})
}

func (t *Transcript) startImageSave(title, name string, data []byte, release func()) {
	t.beginSave(saveRequest{title: title, filename: name, release: release, write: func(w io.Writer) error {
		_, err := w.Write(data)
		return err
	}})
}

type saveRequest struct {
	title, filename string
	release         func()
	write           func(io.Writer) error
}

func (r *saveRequest) releaseSource() {
	r.write = nil
	if r.release != nil {
		r.release()
		r.release = nil
	}
}

func (t *Transcript) beginSave(req saveRequest) {
	if t.deps.Save == nil {
		req.releaseSource()
		t.notice = "Export failed: " + errNoSave.Error()
		return
	}
	ctx, ok := t.exports.claim()
	if !ok {
		req.releaseSource()
		t.notice = "An export is still running; wait for it to finish."
		return
	}
	t.notice = "Choose where to save the " + req.title + "."
	save, wake := saver(t.deps.Save), t.deps.Wake
	go func() {
		status := saveTo(ctx, save, req.title, req.filename, req.write)
		req.releaseSource()
		t.exports.finish(status)
		if wake != nil {
			wake()
		}
	}()
}

func saveTo(ctx context.Context, save saver, title, name string, write func(io.Writer) error) string {
	choice, err := save(ctx, platform.DialogRequest{Kind: platform.DialogSaveFile, Title: "Save " + title, SuggestedName: name})
	if err != nil || choice.Cancelled || len(choice.Files) != 1 {
		return exportOutcome(err, choice)
	}
	if err := ctx.Err(); err != nil {
		return exportOutcome(err, platform.DialogResult{})
	}
	if err := document.SaveFile(ctx, choice.Files[0], write); err != nil {
		return "Export failed; the destination was not replaced: " + err.Error()
	}
	return "Saved the " + title + " to " + choice.Files[0]
}

func (t *Transcript) saveFormula(source, ext string, scale float32) {
	l, err := t.mathLayout(source, float32(theme.SizeBody)*1.2, true)
	if err != nil {
		t.notice = "Export failed: " + err.Error()
		return
	}
	write := l.WriteSVG
	if ext == ".png" {
		write = func(w io.Writer) error { return l.WritePNG(w, max(scale, 1)) }
	}
	t.startSave("formula as "+ext[1:], "formula"+ext, write)
}

func (t *Transcript) saveDiagram(list *figure.List, ext string, scale float32) {
	faces := t.deps.MathText
	if faces == nil {
		t.notice = "Export failed: " + errNoFaces.Error()
		return
	}
	write := func(w io.Writer) error { return figure.WriteSVGWithFaces(w, list, faces, t.deps.MathMono) }
	if ext == ".png" {
		write = func(w io.Writer) error {
			return figure.WritePNGWithFaces(w, list, faces, t.deps.MathMono, max(scale, 1))
		}
	}
	t.startSave("figure as "+ext[1:], "figure"+ext, write)
}
