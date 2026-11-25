//go:build gui

package manage

import (
	"bytes"
	"context"
	"encoding/binary"
	"errors"
	"hash/crc32"
	"image"
	"image/png"
	"testing"

	"gioui.org/f32"
	"gioui.org/io/key"
	"gioui.org/io/pointer"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/terminal"
)

func TestDecodeImageDataRejectsOversizedHeaders(t *testing.T) {
	big := image.NewRGBA(image.Rect(0, 0, 20000, 10))
	var buf bytes.Buffer
	if err := png.Encode(&buf, big); err != nil {
		t.Fatal(err)
	}
	if _, err := decodeImageData(buf.Bytes()); !errors.Is(err, errImageTooLarge) {
		t.Fatalf("20000-pixel-wide image decoded with %v; want the viewer limit", err)
	}
	if _, err := decodeImageData(pngHeader(9000, 9000)); !errors.Is(err, errImageTooLarge) {
		t.Fatalf("81-megapixel image decoded with %v; want the viewer limit", err)
	}
	small := image.NewRGBA(image.Rect(0, 0, 8, 8))
	buf.Reset()
	if err := png.Encode(&buf, small); err != nil {
		t.Fatal(err)
	}
	img, err := decodeImageData(buf.Bytes())
	if err != nil || img.Bounds().Dx() != 8 {
		t.Fatalf("valid 8x8 image failed with %v", err)
	}
	if _, err := decodeImageData([]byte("not an image")); err == nil {
		t.Fatal("garbage bytes decoded without an error")
	}
}

func pngHeader(width, height int) []byte {
	var out []byte
	out = append(out, 0x89, 'P', 'N', 'G', 0x0d, 0x0a, 0x1a, 0x0a)
	var ihdr bytes.Buffer
	_ = binary.Write(&ihdr, binary.BigEndian, uint32(width))  //nolint:gosec // test builds a fixed PNG header with known-small dimensions
	_ = binary.Write(&ihdr, binary.BigEndian, uint32(height)) //nolint:gosec // test builds a fixed PNG header with known-small dimensions
	ihdr.Write([]byte{8, 2, 0, 0, 0})
	out = appendChunk(out, "IHDR", ihdr.Bytes())
	return out
}

func appendChunk(out []byte, kind string, data []byte) []byte {
	var head bytes.Buffer
	_ = binary.Write(&head, binary.BigEndian, uint32(len(data))) //nolint:gosec // test builds a fixed PNG chunk with known-small length
	head.WriteString(kind)
	head.Write(data)
	sum := crc32.ChecksumIEEE(head.Bytes()[4:])
	head.Write([]byte{byte(sum >> 24), byte(sum >> 16), byte(sum >> 8), byte(sum)}) //nolint:gosec // test splits a checksum into bytes
	return append(out, head.Bytes()...)
}

func TestCheckPageEnforcesChunkContinuity(t *testing.T) {
	full := filePage{chunk: readChunk{Offset: "0"}, total: 10, next: 10, data: make([]byte, 10)}
	if err := checkPage(full, 0, 10, 100, 0); err != nil {
		t.Fatalf("complete page rejected: %v", err)
	}
	stalled := filePage{chunk: readChunk{Offset: "0"}, total: 10, next: 0, data: make([]byte, 4)}
	if err := checkPage(stalled, 0, 10, 100, 0); err == nil {
		t.Fatal("stalled cursor accepted as a complete read")
	}
	shifted := filePage{chunk: readChunk{Offset: "4"}, total: 10, next: 10, data: make([]byte, 6)}
	if err := checkPage(shifted, 0, 10, 100, 0); err == nil {
		t.Fatal("page at the wrong offset accepted")
	}
	fat := filePage{chunk: readChunk{Offset: "0"}, total: 10, next: 12, data: make([]byte, 12)}
	if err := checkPage(fat, 0, 10, 100, 0); err == nil {
		t.Fatal("page beyond the declared total accepted")
	}
	moved := filePage{chunk: readChunk{Offset: "0"}, total: 9, next: 9, data: make([]byte, 9)}
	if err := checkPage(moved, 0, 10, 100, 0); err == nil {
		t.Fatal("page with a changed total accepted")
	}
}

func TestMouseEventUsesRealButtonsAndModifiers(t *testing.T) {
	at := terminal.Point{Line: 7, Column: 3}
	right := mouseEvent(pointer.Event{Kind: pointer.Press, Buttons: pointer.ButtonSecondary, Modifiers: key.ModCtrl}, at, 2)
	if right.Button != terminal.MouseRight || right.Action != terminal.MousePress || right.Row != 5 {
		t.Fatalf("right press mapped to %+v", right)
	}
	if right.Modifiers != terminal.ModCtrl {
		t.Fatalf("modifiers mapped to %v", right.Modifiers)
	}
	middle := mouseEvent(pointer.Event{Kind: pointer.Drag, Buttons: pointer.ButtonTertiary}, at, 2)
	if middle.Button != terminal.MouseMiddle || middle.Action != terminal.MouseMotion {
		t.Fatalf("middle drag mapped to %+v", middle)
	}
	release := mouseEvent(pointer.Event{Kind: pointer.Release}, at, 2)
	if release.Button != terminal.MouseLeft || release.Action != terminal.MouseRelease {
		t.Fatalf("release mapped to %+v", release)
	}
}

func TestWheelEventMapsScrollDirection(t *testing.T) {
	at := terminal.Point{Line: 7, Column: 3}
	up := wheelEvent(pointer.Event{Kind: pointer.Scroll, Scroll: f32.Point{Y: -1}}, at, 2)
	if up.Button != terminal.MouseWheelUp || up.Action != terminal.MousePress {
		t.Fatalf("wheel up mapped to %+v", up)
	}
	down := wheelEvent(pointer.Event{Kind: pointer.Scroll, Scroll: f32.Point{Y: 4}}, at, 2)
	if down.Button != terminal.MouseWheelDown {
		t.Fatalf("wheel down mapped to %+v", down)
	}
}

func TestWantReadAccessPreservesReadersAndFinalStates(t *testing.T) {
	final := terminalListing{Status: terminalStatus{State: terminalStateExit}}
	if !wantReadAccess(final, "", true) {
		t.Fatal("finished PTY without a writer requests write")
	}
	busy := terminalListing{Status: terminalStatus{State: "running"}, WriterPresent: true}
	if !wantReadAccess(busy, "", true) {
		t.Fatal("another writer present but write requested")
	}
	free := terminalListing{Status: terminalStatus{State: "running"}}
	if wantReadAccess(free, "", true) {
		t.Fatal("free running PTY requests read")
	}
	if wantReadAccess(free, "lease-1", true) {
		t.Fatal("reconnect with a writer lease requests read")
	}
	if !wantReadAccess(free, "lease-1", false) {
		t.Fatal("reconnect without a writer lease requests write")
	}
}

func TestSubmittableGatesManualCodeWaiting(t *testing.T) {
	a := &authFlow{flow: flowManual, status: &authStatus{State: "waiting"}}
	if !a.submittable() {
		t.Fatal("manual-code login in waiting refuses submission")
	}
	a.status.State = "awaitingInput"
	if !a.submittable() {
		t.Fatal("manual-code login in awaitingInput refuses submission")
	}
	a.flow, a.status.State = flowAPIKey, "waiting"
	if a.submittable() {
		t.Fatal("API-key login in waiting allows submission")
	}
	a.status.State = "awaitingInput"
	if !a.submittable() {
		t.Fatal("API-key login in awaitingInput refuses submission")
	}
	a.flow = flowBrowser
	if a.submittable() {
		t.Fatal("browser login allows submission")
	}
	a.status = nil
	if a.submittable() {
		t.Fatal("missing status allows submission")
	}
}

func TestPickDirtyCurrentFileIsANoOp(t *testing.T) {
	f := &Files{dir: "d"}
	f.doc = newDocument()
	f.doc.path, f.doc.kind = "d/a.txt", docText
	f.doc.loaded = loadedFile{Revision: "r1", Data: []byte("saved")}
	var b binding
	f.pick(b, fileMetadata{Name: "a.txt", Kind: "file", Revision: "r1"})
	if f.moving != moveNone || string(f.doc.loaded.Data) != "saved" {
		t.Fatal("clicking the dirty open file discarded the draft")
	}
	f.doc.path = "d/other.txt"
	f.pick(b, fileMetadata{Name: "a.txt", Kind: "file", Revision: "r1"})
	if f.moving != moveTarget || f.nextFile == nil || f.nextFile.Name != "a.txt" {
		t.Fatal("dirty open file did not route the target change through the save decision")
	}
}

func TestHeldOperationSurvivesResetsUntilResolved(t *testing.T) {
	ctx := context.Background()
	h := NewHub(ctx, Deps{})
	f := &Files{hub: h}
	m := &mutation{}
	m.id, m.method, m.endpoint = "op-1", methodWorkspaceMkdir, client.EndpointID("builder")
	f.settleOwned(lease{}, m, &rpc.UnknownOutcomeError{Method: m.method, OperationID: "op-1"})
	if len(f.held) != 1 || f.held[0].id != "op-1" || f.held[0].endpoint != m.endpoint {
		t.Fatalf("uncertain mutation not held by operation id: %+v", f.held)
	}
	f.op = mutation{}
	f.op.id, f.op.state = "op-2", outcomeRunning
	f.doc.op = mutation{}
	f.releaseSettled(nil)
	if len(f.held) != 1 {
		t.Fatal("unresolved held operation released without a receipt")
	}
	f.op.id, f.op.state = "op-1", outcomeDone
	f.releaseSettled(nil)
	if len(f.held) != 0 {
		t.Fatal("resolved held operation kept its workspace ownership")
	}
}
