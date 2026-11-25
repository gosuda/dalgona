//go:build gui

package platform

import (
	"errors"
	"io"
	"strings"
	"testing"

	"gioui.org/io/clipboard"
	"gioui.org/io/input"
	"gioui.org/io/transfer"
	"gioui.org/op"

	"github.com/gosuda/dalgona/internal/dal/textinput"
)

func TestWriteTextPublishesExactBytes(t *testing.T) {
	var router input.Router
	if err := WriteText(router.Source(), exactSample); err != nil {
		t.Fatal(err)
	}
	mime, content, ok := router.WriteClipboard()
	if !ok || mime != "application/text" || string(content) != exactSample {
		t.Fatalf("clipboard write: ok %v, type %q, content %q; want the exact source text", ok, mime, content)
	}
}

func TestWriteTextRefusesTextTheClipboardCannotCarry(t *testing.T) {
	var router input.Router
	if err := WriteText(router.Source(), "a\x00b"); !errors.Is(err, textinput.DisallowedControl) {
		t.Fatalf("error %v, want DisallowedControl", err)
	}
	if _, content, ok := router.WriteClipboard(); ok {
		t.Fatalf("a refused copy still wrote %q", content)
	}
}

type trackedStream struct {
	io.Reader
	closed int
}

func (s *trackedStream) Close() error {
	s.closed++
	return nil
}

func pasteEvent(mime, data string) (transfer.DataEvent, *trackedStream) {
	stream := &trackedStream{Reader: strings.NewReader(data)}
	return transfer.DataEvent{Type: mime, Open: func() io.ReadCloser { return stream }}, stream
}

func TestReadPasteDeliversTheRequestedTextAndClosesTheStream(t *testing.T) {
	var router input.Router
	tag := new(int)
	router.Source().Execute(clipboard.ReadCmd{Tag: tag})
	router.Frame(new(op.Ops))
	if !router.ClipboardRequested() {
		t.Fatal("the paste request did not reach the platform")
	}
	e, stream := pasteEvent("application/text", exactSample)
	router.Queue(e)
	got, ok := router.Source().Event(PasteFilter(tag))
	if !ok {
		t.Fatal("the paste target received no transfer")
	}
	text, err := ReadPaste(got.(transfer.DataEvent))
	if err != nil || text != exactSample {
		t.Fatalf("paste text %q, error %v; want the exact transfer", text, err)
	}
	if stream.closed != 1 {
		t.Fatalf("transfer stream closed %d times, want once", stream.closed)
	}
}

func TestReadPasteClosesRejectedStreams(t *testing.T) {
	cases := []struct {
		mime, data string
		want       error
	}{
		{"application/text", "bad\x00text", textinput.DisallowedControl},
		{"application/text", "bad\xfftext", textinput.InvalidEncoding},
		{"image/png", "\x89PNG", ErrUnsupportedPaste},
	}
	for _, tc := range cases {
		e, stream := pasteEvent(tc.mime, tc.data)
		text, err := ReadPaste(e)
		if !errors.Is(err, tc.want) || text != "" {
			t.Errorf("%s %q: text %q, error %v; want %v", tc.mime, tc.data, text, err, tc.want)
		}
		if tc.mime == "application/text" && stream.closed != 1 {
			t.Errorf("%q: stream closed %d times, want once", tc.data, stream.closed)
		}
	}
}
