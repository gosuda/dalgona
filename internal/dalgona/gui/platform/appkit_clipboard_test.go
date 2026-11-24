package platform

import (
	"errors"
	"slices"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
)

func TestAppKitPasteboardDecodesEachFormat(t *testing.T) {
	text, err := decodeAppKitPasteboard(ClipboardText, []byte("line\r\n😀\n"))
	if err != nil || text.Text != "line\r\n😀\n" {
		t.Fatalf("text %+v, %v; want the exact scalars", text, err)
	}
	png := []byte("\x89PNG\r\n\x1a\n")
	image, err := decodeAppKitPasteboard(ClipboardImage, png)
	png[0] = 0
	if err != nil || image.ImageType != "image/png" || image.Image[0] != 0x89 {
		t.Fatalf("image %+v, %v; want a PNG copy independent of the native buffer", image, err)
	}
	list := "file:///Users/me/My%20Notes.md\r\nfile:///Users/me/%ED%95%9C%EA%B8%80.png\r\n"
	files, err := decodeAppKitPasteboard(ClipboardFiles, []byte(list))
	if want := []string{"/Users/me/My Notes.md", "/Users/me/한글.png"}; err != nil || !slices.Equal(files.Files, want) {
		t.Fatalf("files %v, %v; want %v in pasteboard order", files.Files, err, want)
	}
}

func TestAppKitPasteboardRefusesUnsafeContent(t *testing.T) {
	cases := []struct {
		name   string
		format ClipboardFormats
		data   []byte
		want   error
	}{
		{"text with NUL", ClipboardText, []byte("a\x00b"), inputtext.DisallowedControl},
		{"malformed UTF-8", ClipboardText, []byte{'a', 0xC0, 0x80}, inputtext.InvalidEncoding},
		{"oversize text", ClipboardText, make([]byte, transferLimit+1), ErrTransferTooLarge},
		{"remote URL", ClipboardFiles, []byte("file:///tmp/a\r\nhttps://example.com/b\r\n"), ErrNotLocalFile},
		{"empty image", ClipboardImage, nil, ErrUnsupportedPaste},
	}
	for _, tc := range cases {
		if _, err := decodeAppKitPasteboard(tc.format, tc.data); !errors.Is(err, tc.want) {
			t.Errorf("%s: %v, want %v", tc.name, err, tc.want)
		}
	}
}

func TestAppKitPasteboardStatusNamesTheFailure(t *testing.T) {
	cases := map[int]error{
		appKitPasteboardRead:          nil,
		appKitPasteboardChanged:       errPasteboardChanged,
		appKitPasteboardTooLarge:      ErrTransferTooLarge,
		appKitPasteboardNotLocal:      ErrNotLocalFile,
		appKitPasteboardTooManyPixels: errPasteboardTooManyPixels,
		appKitPasteboardOutOfMemory:   errPasteboardOutOfMemory,
		-1:                            ErrUnsupportedPaste,
		99:                            ErrUnsupportedPaste,
	}
	for status, want := range cases {
		if got := appKitPasteboardError(status); !errors.Is(got, want) || (want == nil) != (got == nil) {
			t.Errorf("status %d: %v, want %v", status, got, want)
		}
	}
}
