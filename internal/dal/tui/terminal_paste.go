package tui

import (
	"fmt"
	"io"
	"os"
	"strings"
	"sync"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/term"
)

const (
	pasteStartMarker = "\x1b[200~"
	pasteEndMarker   = "\x1b[201~"
	pasteMarkerGap   = 50 * time.Millisecond
)

type terminalInput struct {
	stream io.Reader
	paste  *terminalPasteReader
	close  func()
}

type terminalInputFile interface {
	term.File
	Name() string
}

type terminalPasteFile struct {
	terminalInputFile
	paste *terminalPasteReader
}

func (f terminalPasteFile) Read(p []byte) (int, error) {
	return f.paste.Read(p)
}

func openTerminalInput(source io.Reader) (terminalInput, error) {
	input := terminalInput{close: func() {}}
	if source == nil && !term.IsTerminal(os.Stdin.Fd()) {
		in, out, err := tea.OpenTTY()
		if err != nil {
			return terminalInput{}, fmt.Errorf("open tui terminal: %w", err)
		}
		source = in
		input.close = func() { _ = in.Close(); _ = out.Close() }
	}
	if source == nil {
		source = os.Stdin
	}
	input.paste = &terminalPasteReader{source: source}
	input.stream = input.paste
	if file, ok := source.(terminalInputFile); ok {
		input.stream = terminalPasteFile{terminalInputFile: file, paste: input.paste}
	}
	return input, nil
}

type terminalPasteRefused struct{}

type terminalPasteReader struct {
	source    io.Reader
	opened    bool
	prefix    int
	prefixAt  time.Time
	body      strings.Builder
	oversized bool

	mu      sync.Mutex
	ready   []tea.Msg
	awaited bool
}

func (r *terminalPasteReader) Read(p []byte) (int, error) {
	const room = len(pasteEndMarker) - 1
	if len(p) <= room {
		return 0, io.ErrShortBuffer
	}
	n, err := r.source.Read(p[room:])
	if !r.opened && r.prefix != 0 && time.Since(r.prefixAt) > pasteMarkerGap {
		r.prefix = 0
	}
	out := p[:0]
	for _, b := range p[room : room+n] {
		out = r.scan(out, b)
	}
	if !r.opened && r.prefix != 0 {
		r.prefixAt = time.Now()
	}
	return len(out), err
}

func (r *terminalPasteReader) scan(out []byte, b byte) []byte {
	if !r.opened {
		r.scanStart(b)
		return append(out, b)
	}
	if b == pasteEndMarker[r.prefix] {
		r.prefix++
		if r.prefix == len(pasteEndMarker) {
			r.completePaste()
			return append(out, pasteEndMarker...)
		}
		return out
	}
	for i := range r.prefix {
		r.retain(pasteEndMarker[i])
	}
	r.prefix = 0
	if b == pasteEndMarker[0] {
		r.prefix = 1
		return out
	}
	r.retain(b)
	return out
}

func (r *terminalPasteReader) scanStart(b byte) {
	switch b {
	case pasteStartMarker[r.prefix]:
		r.prefix++
		if r.prefix == len(pasteStartMarker) {
			r.opened, r.prefix = true, 0
		}
	case pasteStartMarker[0]:
		r.prefix = 1
	default:
		r.prefix = 0
	}
}

func (r *terminalPasteReader) retain(b byte) {
	if r.oversized {
		return
	}
	if r.body.Len() == pasteLimit {
		r.oversized = true
		r.body.Reset()
		return
	}
	_ = r.body.WriteByte(b)
}

func (r *terminalPasteReader) completePaste() {
	var msg tea.Msg = terminalPasteRefused{}
	if !r.oversized {
		msg = tea.PasteMsg{Content: r.body.String()}
	}
	r.opened, r.oversized, r.prefix = false, false, 0
	r.body.Reset()
	r.mu.Lock()
	defer r.mu.Unlock()
	r.ready = append(r.ready, msg)
}

func (r *terminalPasteReader) restore(_ tea.Model, msg tea.Msg) tea.Msg {
	if _, ok := msg.(tea.PasteStartMsg); ok {
		r.awaited = true
		return msg
	}
	paste, ok := msg.(tea.PasteMsg)
	if !ok {
		return msg
	}
	awaited := r.awaited
	r.awaited = false
	if !awaited || paste.Content != "" {
		return msg
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	if len(r.ready) == 0 {
		return msg
	}
	ready := r.ready[0]
	r.ready[0] = nil
	if len(r.ready) == 1 {
		r.ready = r.ready[:0]
	} else {
		r.ready = r.ready[1:]
	}
	return ready
}
