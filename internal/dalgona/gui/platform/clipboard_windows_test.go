//go:build gui && guiintegration

package platform

import (
	"bytes"
	"context"
	"encoding/binary"
	"errors"
	"fmt"
	"image"
	"image/color"
	"image/png"
	"runtime"
	"slices"
	"sync"
	"testing"
	"time"
	"unicode/utf16"
	"unsafe"

	"github.com/gosuda/dalgona/internal/dal/textinput"
	"github.com/gosuda/dalgona/internal/dalgona/gui/clipboardimage"
)

const (
	testClipboardMoveable  = 0x0002
	testClipboardDropFiles = 20
)

var (
	procTestCreateWindowExW  = clipboardUser32.NewProc("CreateWindowExW")
	procTestDestroyWindow    = clipboardUser32.NewProc("DestroyWindow")
	procTestEmptyClipboard   = clipboardUser32.NewProc("EmptyClipboard")
	procTestSetClipboardData = clipboardUser32.NewProc("SetClipboardData")
	procTestGlobalAlloc      = clipboardKernel32.NewProc("GlobalAlloc")
	procTestGlobalFree       = clipboardKernel32.NewProc("GlobalFree")
	testStaticWindowClass    = utf16.Encode([]rune("STATIC\x00"))
)

type testClipboardEntry struct {
	format uintptr
	data   []byte
}

func unicodeTextEntry(units ...uint16) testClipboardEntry {
	data := make([]byte, 0, 2*len(units))
	for _, unit := range units {
		data = binary.LittleEndian.AppendUint16(data, unit)
	}
	return testClipboardEntry{format: clipboardFormatUnicodeText, data: data}
}

func dropFilesEntry(files ...string) testClipboardEntry {
	data := binary.LittleEndian.AppendUint32(nil, testClipboardDropFiles)
	data = append(data, make([]byte, 12)...)
	data = binary.LittleEndian.AppendUint32(data, 1)
	for _, file := range files {
		for _, unit := range utf16.Encode([]rune(file + "\x00")) {
			data = binary.LittleEndian.AppendUint16(data, unit)
		}
	}
	return testClipboardEntry{format: clipboardFormatHDrop, data: binary.LittleEndian.AppendUint16(data, 0)}
}

func writeTestClipboard(t *testing.T, entries ...testClipboardEntry) {
	t.Helper()
	written := make(chan error, 1)
	go func() { written <- writeTestClipboardOnThread(entries) }()
	if err := <-written; err != nil {
		t.Fatal(err)
	}
}

func writeTestClipboardOnThread(entries []testClipboardEntry) error {
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	window, _, err := procTestCreateWindowExW.Call(0, uintptr(unsafe.Pointer(&testStaticWindowClass[0])), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0) //nolint:gosec // CreateWindowExW reads the class name through a raw pointer
	if window == 0 {
		return fmt.Errorf("create the clipboard owner window: %w", err)
	}
	defer func() { _, _, _ = procTestDestroyWindow.Call(window) }()
	if opened, _, err := procOpenClipboard.Call(window); opened&clipboardUintBits == 0 {
		return fmt.Errorf("open the clipboard for writing: %w", err)
	}
	writeErr := placeTestClipboardEntries(entries)
	if closed, _, err := procCloseClipboard.Call(); closed&clipboardUintBits == 0 {
		return errors.Join(writeErr, fmt.Errorf("close the clipboard after writing: %w", err))
	}
	return writeErr
}

func placeTestClipboardEntries(entries []testClipboardEntry) error {
	if emptied, _, err := procTestEmptyClipboard.Call(); emptied&clipboardUintBits == 0 {
		return fmt.Errorf("empty the clipboard: %w", err)
	}
	for _, entry := range entries {
		handle, _, err := procTestGlobalAlloc.Call(testClipboardMoveable, uintptr(len(entry.data))) //nolint:gosec // a test entry length is never negative
		if handle == 0 {
			return fmt.Errorf("allocate clipboard format %d: %w", entry.format, err)
		}
		target, _, err := procClipboardGlobalLock.Call(handle)
		if target == 0 {
			return fmt.Errorf("lock clipboard format %d: %w", entry.format, err)
		}
		_, _, _ = procClipboardMoveMemory.Call(target, uintptr(unsafe.Pointer(unsafe.SliceData(entry.data))), uintptr(len(entry.data))) //nolint:gosec // RtlMoveMemory reads the entry bytes through a raw pointer
		_, _, _ = procClipboardGlobalUnlock.Call(handle)
		if placed, _, err := procTestSetClipboardData.Call(entry.format, handle); placed == 0 {
			_, _, _ = procTestGlobalFree.Call(handle)
			return fmt.Errorf("place clipboard format %d: %w", entry.format, err)
		}
	}
	return nil
}

func readTestClipboard(t *testing.T, kind PasteKind) (ClipboardContent, error) {
	t.Helper()
	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
	defer cancel()
	return NewWindowsClipboard().ReadClipboard(ctx, kind)
}

func TestWindowsClipboardKeepsExactUnicodeText(t *testing.T) {
	recordGate(t, GateClipboard, "CF_UNICODETEXT keeps CRLF, combining marks, and surrogate pairs, ignores NUL padding after the terminator, and refuses text after a NUL and unpaired surrogates")
	text := "line one\r\nline two\r\ne\u0301 \U0001F469\u200D\U0001F4BB"
	writeTestClipboard(t, unicodeTextEntry(append(utf16.Encode([]rune(text)), 0)...))
	if content, err := readTestClipboard(t, PasteOrdinary); err != nil || content.Text != text {
		t.Fatalf("read %q, %v; want %q", content.Text, err, text)
	}
	writeTestClipboard(t, unicodeTextEntry('p', 'a', 'd', 0, 0, 0, 0))
	if content, err := readTestClipboard(t, PasteOrdinary); err != nil || content.Text != "pad" {
		t.Fatalf("a padded block read %q, %v; want %q", content.Text, err, "pad")
	}
	refused := []struct {
		name  string
		units []uint16
		want  error
	}{
		{"text after a NUL", []uint16{'a', 0, 'b', 0}, textinput.DisallowedControl},
		{"unpaired high surrogate", []uint16{'a', 0xD83D, 'b', 0}, textinput.InvalidEncoding},
		{"lone low surrogate", []uint16{0xDE00, 0}, textinput.InvalidEncoding},
	}
	for _, tc := range refused {
		writeTestClipboard(t, unicodeTextEntry(tc.units...))
		if content, err := readTestClipboard(t, PasteOrdinary); !errors.Is(err, tc.want) {
			t.Errorf("%s: read %q, %v; want %v", tc.name, content.Text, err, tc.want)
		}
	}
}

func TestWindowsClipboardConvertsBitmapsAndKeepsPNG(t *testing.T) {
	recordGate(t, GateClipboard, "CF_DIBV5 alpha and CF_DIB pixels arrive as PNG, and the registered PNG format arrives unchanged")
	v5 := dibFixture{
		headerSize: dibV5HeaderSize, width: 2, height: 1, planes: 1, bitCount: 32,
		compression: dibCompressionBitfields, masks: dibStandardMasks(0xFF000000),
		pixels: dibDWORDRow(0x80FF0000, 0xFF123456),
	}
	writeTestClipboard(t, testClipboardEntry{format: clipboardFormatDIBV5, data: v5.encode()})
	checkClipboardImage(t, []color.NRGBA{{R: 0xFF, A: 0x80}, dibOpaque(dibMixed)})
	info := dibFixture{
		headerSize: dibInfoHeaderSize, width: 2, height: 1, planes: 1, bitCount: 24,
		pixels: dibBGRRow(2, dibBlue, dibMixed),
	}
	writeTestClipboard(t, testClipboardEntry{format: clipboardFormatDIB, data: info.encode()})
	checkClipboardImage(t, []color.NRGBA{dibOpaque(dibBlue), dibOpaque(dibMixed)})
	var encoded bytes.Buffer
	if err := png.Encode(&encoded, image.NewNRGBA(image.Rect(0, 0, 3, 2))); err != nil {
		t.Fatal(err)
	}
	pngFormat, _, err := procRegisterClipboardFormatW.Call(uintptr(unsafe.Pointer(&clipboardPNGFormatName[0]))) //nolint:gosec // RegisterClipboardFormatW reads the format name through a raw pointer
	if pngFormat&clipboardUintBits == 0 {
		t.Fatal(err)
	}
	writeTestClipboard(t, testClipboardEntry{format: pngFormat & clipboardUintBits, data: encoded.Bytes()}, unicodeTextEntry('x', 0))
	content, err := readTestClipboard(t, PasteImage)
	if err != nil || !bytes.Equal(content.Image, encoded.Bytes()) || content.ImageType != "image/png" {
		t.Fatalf("Paste image read %d bytes of %q, %v; want the %d placed PNG bytes", len(content.Image), content.ImageType, err, encoded.Len())
	}
	if content, err := readTestClipboard(t, PasteOrdinary); err != nil || content.Text != "x" {
		t.Fatalf("Paste read %q, %v; want the text beside the image", content.Text, err)
	}
}

func checkClipboardImage(t *testing.T, want []color.NRGBA) {
	t.Helper()
	content, err := readTestClipboard(t, PasteImage)
	if err != nil {
		t.Fatal(err)
	}
	img, err := png.Decode(bytes.NewReader(content.Image))
	if err != nil {
		t.Fatalf("the pasted image is not a PNG: %v", err)
	}
	if img.Bounds() != image.Rect(0, 0, len(want), 1) {
		t.Fatalf("bounds %v, want %d×1", img.Bounds(), len(want))
	}
	for x, pixel := range want {
		if got := color.NRGBAModel.Convert(img.At(x, 0)); got != pixel {
			t.Errorf("pixel %d = %#v, want %#v", x, got, pixel)
		}
	}
}

func TestWindowsClipboardListsCopiedFilesInOrder(t *testing.T) {
	recordGate(t, GateClipboard, "CF_HDROP file names arrive in copy order with spaces and non-ASCII characters intact")
	files := []string{`C:\dalgona test\second file.txt`, `C:\dalgona test\한글 파일.png`, `\\server\share\a#b%20c.txt`}
	writeTestClipboard(t, dropFilesEntry(files...))
	content, err := readTestClipboard(t, PasteOrdinary)
	if err != nil || !slices.Equal(content.Files, files) {
		t.Fatalf("read %q, %v; want %q", content.Files, err, files)
	}
}

func TestWindowsClipboardWritesPNGAndDIB(t *testing.T) {
	recordGate(t, GateClipboard, "a staged native offer publishes exact PNG bytes and matching CF_DIB")
	placed := testPNG(t, 3, 2)
	publishTestWindowsImage(t, placed)
	content, err := readTestClipboard(t, PasteImage)
	if err != nil || !bytes.Equal(content.Image, placed) || content.ImageType != "image/png" {
		t.Fatalf("pasted %d bytes of %q with %v, want the %d placed PNG bytes", len(content.Image), content.ImageType, err, len(placed))
	}
	checkClipboardDIB(t, placed)
}

func publishTestWindowsImage(t *testing.T, data []byte) {
	t.Helper()
	img, err := png.Decode(bytes.NewReader(data))
	if err != nil {
		t.Fatal(err)
	}
	dib := dibFixture{
		headerSize: dibV4HeaderSize, width: 3, height: 2, planes: 1, bitCount: 32,
		compression: dibCompressionBitfields, masks: dibStandardMasks(0xFF000000),
	}
	for y := 1; y >= 0; y-- {
		for x := range 3 {
			pixel, ok := color.NRGBAModel.Convert(img.At(x, y)).(color.NRGBA)
			if !ok {
				t.Fatal("fixture pixel did not convert to NRGBA")
			}
			dib.pixels = append(dib.pixels, pixel.B, pixel.G, pixel.R, pixel.A)
		}
	}
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	owner, _, ownerErr := procTestCreateWindowExW.Call(0, uintptr(unsafe.Pointer(&testStaticWindowClass[0])), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0) //nolint:gosec // CreateWindowExW reads the live test class name.
	if owner == 0 {
		t.Fatal(ownerErr)
	}
	defer func() { _, _, _ = procTestDestroyWindow.Call(owner) }()
	offer := NewWindowsClipboardOffer(t.Context(), owner)
	defer offer.Close()
	bitmap := dib.encode()
	p, d, err := offer.Stage(clipboardimage.Spec{Size: image.Pt(3, 2), PNGBytes: len(data), DIBBytes: len(bitmap)})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := p.Write(data); err != nil {
		t.Fatal(err)
	}
	if _, err := d.Write(bitmap); err != nil {
		t.Fatal(err)
	}
	if err := offer.Publish(); err != nil {
		t.Fatal(err)
	}
}

func checkClipboardDIB(t *testing.T, placed []byte) {
	t.Helper()
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
	defer cancel()
	if err := openClipboard(ctx); err != nil {
		t.Fatal(err)
	}
	defer func() {
		if closed, _, err := procCloseClipboard.Call(); closed&clipboardUintBits == 0 {
			t.Errorf("close the clipboard after the DIB check: %v", err)
		}
	}()
	format, err := pngClipboardFormat()
	if err != nil {
		t.Fatal(err)
	}
	if !clipboardHolds(format) || !clipboardHolds(clipboardFormatDIB) {
		t.Fatalf("the clipboard holds PNG %v and DIB %v, want both formats", clipboardHolds(format), clipboardHolds(clipboardFormatDIB))
	}
	dib, err := copyClipboardBytes(clipboardFormatDIB)
	if err != nil {
		t.Fatal(err)
	}
	decoded, err := pngFromDIB(dib)
	if err != nil {
		t.Fatal(err)
	}
	want, err := png.Decode(bytes.NewReader(placed))
	if err != nil {
		t.Fatal(err)
	}
	got, err := png.Decode(bytes.NewReader(decoded))
	if err != nil {
		t.Fatalf("the placed DIB did not decode as a PNG: %v", err)
	}
	if got.Bounds() != want.Bounds() {
		t.Fatalf("the placed DIB covers %v, want %v", got.Bounds(), want.Bounds())
	}
	for y := want.Bounds().Min.Y; y < want.Bounds().Max.Y; y++ {
		for x := want.Bounds().Min.X; x < want.Bounds().Max.X; x++ {
			gotPixel, wantPixel := color.NRGBAModel.Convert(got.At(x, y)), color.NRGBAModel.Convert(want.At(x, y))
			if gotPixel != wantPixel {
				t.Fatalf("DIB pixel (%d,%d) is %v, want %v", x, y, gotPixel, wantPixel)
			}
		}
	}
}

func TestWindowsClipboardWaitsForAnotherHolder(t *testing.T) {
	recordGate(t, GateClipboard, "a paste waits while another thread holds the clipboard and stops when its context ends")
	writeTestClipboard(t, unicodeTextEntry('h', 'e', 'l', 'd', 0))
	release := holdTestClipboard(t)
	go func() {
		time.Sleep(200 * time.Millisecond)
		release()
	}()
	if content, err := readTestClipboard(t, PasteOrdinary); err != nil || content.Text != "held" {
		t.Fatalf("read %q, %v after the holder closed the clipboard", content.Text, err)
	}
	release = holdTestClipboard(t)
	defer release()
	ctx, cancel := context.WithTimeout(t.Context(), 50*time.Millisecond)
	defer cancel()
	if _, err := NewWindowsClipboard().ReadClipboard(ctx, PasteOrdinary); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("a held clipboard returned %v; want the context deadline", err)
	}
}

func holdTestClipboard(t *testing.T) func() {
	t.Helper()
	opened := make(chan error, 1)
	done := make(chan struct{})
	closed := make(chan struct{})
	go func() {
		runtime.LockOSThread()
		defer runtime.UnlockOSThread()
		defer close(closed)
		if ok, _, err := procOpenClipboard.Call(0); ok&clipboardUintBits == 0 {
			opened <- fmt.Errorf("hold the clipboard: %w", err)
			return
		}
		opened <- nil
		<-done
		_, _, _ = procCloseClipboard.Call()
	}()
	if err := <-opened; err != nil {
		t.Fatal(err)
	}
	return sync.OnceFunc(func() {
		close(done)
		<-closed
	})
}
