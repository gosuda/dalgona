//go:build linux || freebsd || openbsd

package platform

import (
	"bytes"
	"context"
	"encoding/binary"
	"errors"
	"image"
	"net"
	"slices"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
)

const (
	x11TestOpChangeWindowAttributes = 2
	x11TestOpSetSelectionOwner      = 22
	x11TestOpGetSelectionOwner      = 23
	x11TestOpSendEvent              = 25
	x11TestEventSelectionRequest    = 30
	x11TestPropertyDeleted          = 1
	x11TestModeReplace              = 0
	x11TestAtomAtom                 = 4
	x11TestWindowInputOutput        = 1
	x11TestWindowError              = 3
)

type x11Offer struct {
	target string
	data   []byte
}

type x11OwnerPolicy struct {
	stall         bool
	refuse        bool
	storeWhole    bool
	hideSize      bool
	corruptChunks bool
}

type x11SelectionRequest struct {
	time      uint32
	requestor uint32
	selection uint32
	target    uint32
	property  uint32
}

type x11TestOwner struct {
	conn      *x11Conn
	policy    x11OwnerPolicy
	clipboard uint32
	targets   uint32
	incr      uint32
	offers    map[uint32][]byte
	requests  chan uint32
	chunks    atomic.Int64
	done      chan struct{}
	failure   error
}

func startX11Owner(t *testing.T, display string, policy x11OwnerPolicy, offers ...x11Offer) *x11TestOwner {
	t.Helper()
	conn, err := dialX11(display)
	if err != nil {
		t.Fatal(err)
	}
	names := []string{"CLIPBOARD", "TARGETS", "INCR"}
	for _, offer := range offers {
		names = append(names, offer.target)
	}
	atoms, err := conn.internAtoms(names)
	if err != nil {
		t.Fatal(err)
	}
	owner := &x11TestOwner{
		conn:      conn,
		policy:    policy,
		clipboard: atoms[0],
		targets:   atoms[1],
		incr:      atoms[2],
		offers:    map[uint32][]byte{},
		requests:  make(chan uint32, 64),
		done:      make(chan struct{}),
	}
	for i, offer := range offers {
		owner.offers[atoms[3+i]] = offer.data
	}
	owner.own(t)
	go owner.serve()
	t.Cleanup(func() {
		_ = conn.close()
		<-owner.done
		if !errors.Is(owner.failure, net.ErrClosed) {
			t.Errorf("the test selection owner failed: %v", owner.failure)
		}
	})
	return owner
}

func (o *x11TestOwner) own(t *testing.T) {
	t.Helper()
	window, err := o.conn.createInputWindow(0)
	if err != nil {
		t.Fatal(err)
	}
	request := binary.LittleEndian.AppendUint32(o.conn.begin(x11TestOpSetSelectionOwner, 0), window)
	request = binary.LittleEndian.AppendUint32(request, o.clipboard)
	if err = o.conn.send(binary.LittleEndian.AppendUint32(request, 0)); err != nil {
		t.Fatal(err)
	}
	reply, err := o.conn.roundTrip(binary.LittleEndian.AppendUint32(o.conn.begin(x11TestOpGetSelectionOwner, 0), o.clipboard))
	if err != nil {
		t.Fatal(err)
	}
	if owner := binary.LittleEndian.Uint32(reply[8:]); owner != window {
		t.Fatalf("CLIPBOARD is owned by %#x, want the test owner %#x", owner, window)
	}
}

func (o *x11TestOwner) chunkSize() int {
	return 4*o.conn.maxRequestUnits - 24
}

func (o *x11TestOwner) serve() {
	defer close(o.done)
	for {
		event, err := o.conn.nextEvent()
		if err != nil {
			o.failure = err
			return
		}
		if event[0]&x11EventCodeMask != x11TestEventSelectionRequest {
			continue
		}
		request := x11SelectionRequest{
			time:      binary.LittleEndian.Uint32(event[4:]),
			requestor: binary.LittleEndian.Uint32(event[12:]),
			selection: binary.LittleEndian.Uint32(event[16:]),
			target:    binary.LittleEndian.Uint32(event[20:]),
			property:  binary.LittleEndian.Uint32(event[24:]),
		}
		select {
		case o.requests <- request.target:
		default:
		}
		if o.policy.stall {
			continue
		}
		if err := o.answer(request); err != nil {
			o.failure = err
			return
		}
	}
}

func (o *x11TestOwner) answer(r x11SelectionRequest) error {
	data, offered := o.offers[r.target]
	switch {
	case r.target == o.targets:
		list := binary.LittleEndian.AppendUint32(nil, o.targets)
		for atom := range o.offers {
			list = binary.LittleEndian.AppendUint32(list, atom)
		}
		return o.store(r, x11TestAtomAtom, 32, list)
	case !offered || o.policy.refuse:
		return o.notify(r, 0)
	case len(data) <= o.chunkSize() || o.policy.storeWhole:
		return o.store(r, r.target, 8, data)
	default:
		return o.sendIncrementally(r, data)
	}
}

func (o *x11TestOwner) store(r x11SelectionRequest, kind uint32, format byte, data []byte) error {
	first := min(len(data), o.chunkSize())
	write := x11PropertyWrite{window: r.requestor, property: r.property, kind: kind, mode: x11TestModeReplace, format: format, data: data[:first]}
	if err := o.conn.changeProperty(write); err != nil {
		return err
	}
	write.mode = x11ModeAppend
	for chunk := range slices.Chunk(data[first:], o.chunkSize()) {
		write.data = chunk
		if err := o.conn.changeProperty(write); err != nil {
			return err
		}
	}
	if _, err := o.conn.roundTrip(binary.LittleEndian.AppendUint32(o.conn.begin(x11TestOpGetSelectionOwner, 0), o.clipboard)); err != nil {
		return err
	}
	return o.notify(r, r.property)
}

func (o *x11TestOwner) sendIncrementally(r x11SelectionRequest, data []byte) error {
	request := binary.LittleEndian.AppendUint32(o.conn.begin(x11TestOpChangeWindowAttributes, 0), r.requestor)
	request = binary.LittleEndian.AppendUint32(request, x11ValueEventMask)
	if err := o.conn.send(binary.LittleEndian.AppendUint32(request, x11PropertyChangeMask)); err != nil {
		return err
	}
	size := uint32(len(data)) //nolint:gosec // G115: test transfers stay below 64 MiB
	if o.policy.hideSize {
		size = 0
	}
	write := x11PropertyWrite{window: r.requestor, property: r.property, kind: o.incr, mode: x11TestModeReplace, format: 32, data: binary.LittleEndian.AppendUint32(nil, size)}
	if err := o.conn.changeProperty(write); err != nil {
		return err
	}
	if err := o.notify(r, r.property); err != nil {
		return err
	}
	write.kind, write.mode, write.format = r.target, x11ModeAppend, 8
	for chunk := range slices.Chunk(data, o.chunkSize()) {
		write.data = chunk
		if o.policy.corruptChunks && o.chunks.Load() > 0 {
			write.kind = o.targets
		}
		if err := o.sendChunk(r, write); err != nil {
			return err
		}
	}
	write.data = nil
	return o.endIncremental(r, write)
}

func (o *x11TestOwner) endIncremental(r x11SelectionRequest, write x11PropertyWrite) error {
	if err := o.awaitDeleted(r); err != nil {
		return err
	}
	write.data = nil
	return o.conn.changeProperty(write)
}

func (o *x11TestOwner) sendChunk(r x11SelectionRequest, write x11PropertyWrite) error {
	if err := o.awaitDeleted(r); err != nil {
		return err
	}
	o.chunks.Add(1)
	return o.conn.changeProperty(write)
}

func (o *x11TestOwner) awaitDeleted(r x11SelectionRequest) error {
	for {
		event, err := o.conn.nextEvent()
		if err != nil {
			return err
		}
		deleted := event[0]&x11EventCodeMask == x11EventPropertyNotify && event[16] == x11TestPropertyDeleted
		if deleted && binary.LittleEndian.Uint32(event[4:]) == r.requestor && binary.LittleEndian.Uint32(event[8:]) == r.property {
			return nil
		}
	}
}

func (o *x11TestOwner) notify(r x11SelectionRequest, property uint32) error {
	request := binary.LittleEndian.AppendUint32(o.conn.begin(x11TestOpSendEvent, 0), r.requestor)
	request = binary.LittleEndian.AppendUint32(request, 0)
	request = append(request, x11EventSelectionNotify, 0, 0, 0)
	for _, field := range [...]uint32{r.time, r.requestor, r.selection, r.target, property} {
		request = binary.LittleEndian.AppendUint32(request, field)
	}
	return o.conn.send(append(request, 0, 0, 0, 0, 0, 0, 0, 0))
}

type x11TestWindow struct {
	parent uint32
	x, y   int16
	border uint16
}

func createX11TestWindow(t *testing.T, conn *x11Conn, spec x11TestWindow) uint32 {
	t.Helper()
	window := conn.newID()
	request := binary.LittleEndian.AppendUint32(conn.begin(x11OpCreateWindow, 0), window)
	request = binary.LittleEndian.AppendUint32(request, spec.parent)
	geometry := struct {
		X, Y                         int16
		Width, Height, Border, Class uint16
	}{spec.x, spec.y, 30, 20, spec.border, x11TestWindowInputOutput}
	request, err := binary.Append(request, binary.LittleEndian, geometry)
	if err != nil {
		t.Fatal(err)
	}
	if err := conn.send(append(request, 0, 0, 0, 0, 0, 0, 0, 0)); err != nil {
		t.Fatal(err)
	}
	if _, err := conn.internAtoms([]string{"PRIMARY"}); err != nil {
		t.Fatal(err)
	}
	return window
}

func openX11ClipboardForTest(t *testing.T, display string) *X11Clipboard {
	t.Helper()
	clipboard, err := OpenX11Clipboard(display)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := clipboard.Close(); err != nil {
			t.Error(err)
		}
	})
	return clipboard
}

func readX11Clipboard(t *testing.T, clipboard *X11Clipboard, kind PasteKind) (ClipboardContent, error) {
	t.Helper()
	ctx, cancel := context.WithTimeout(t.Context(), time.Minute)
	defer cancel()
	return clipboard.ReadClipboard(ctx, kind)
}

func sameClipboardContent(a, b ClipboardContent) bool {
	return a.Text == b.Text && bytes.Equal(a.Image, b.Image) && a.ImageType == b.ImageType && slices.Equal(a.Files, b.Files)
}

func x11Pattern(size int) []byte {
	data := make([]byte, size)
	var next byte
	for i := range data {
		data[i] = next
		next = (next + 1) % 251
	}
	return data
}

func TestX11ClipboardReadsOfferedTextExactly(t *testing.T) {
	display := startX11Server(t, withoutTCP)
	clipboard := openX11ClipboardForTest(t, display)
	cases := []struct {
		target string
		text   string
	}{
		{"UTF8_STRING", exactSample},
		{"text/plain;charset=utf-8", exactSample},
		{"UTF8_STRING", "first\r\nsecond\r\n"},
		{"UTF8_STRING", "e\u0301\u2028\U0001F44D\U0001F3FD\u200D"},
	}
	for _, c := range cases {
		startX11Owner(t, display, x11OwnerPolicy{}, x11Offer{target: "STRING", data: []byte("latin")}, x11Offer{target: c.target, data: []byte(c.text)})
		got, err := readX11Clipboard(t, clipboard, PasteOrdinary)
		if err != nil {
			t.Fatalf("%s: %v", c.target, err)
		}
		if !sameClipboardContent(got, ClipboardContent{Text: c.text}) {
			t.Fatalf("%s pasted %+q, want exactly %+q", c.target, got, c.text)
		}
	}
}

func TestX11ClipboardChoosesTheFormatForThePasteKind(t *testing.T) {
	display := startX11Server(t, withoutTCP)
	clipboard := openX11ClipboardForTest(t, display)
	png := []byte("\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR")
	jpeg := []byte{0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 'J', 'F', 'I', 'F', 0x00}
	uriList := "file:///tmp/b%20c\r\nfile:///tmp/a\r\n# comment\r\nfile:///tmp/%ED%95%9C\r\n"
	cases := []struct {
		name   string
		offers []x11Offer
		kind   PasteKind
		want   ClipboardContent
	}{
		{"ordinary paste prefers text", []x11Offer{{"image/png", png}, {"UTF8_STRING", []byte("caption")}}, PasteOrdinary, ClipboardContent{Text: "caption"}},
		{"image paste prefers the image", []x11Offer{{"UTF8_STRING", []byte("caption")}, {"image/png", png}}, PasteImage, ClipboardContent{Image: png, ImageType: "image/png"}},
		{"an image-only clipboard pastes the image", []x11Offer{{"image/jpeg", jpeg}}, PasteOrdinary, ClipboardContent{Image: jpeg, ImageType: "image/jpeg"}},
		{"png wins over jpeg", []x11Offer{{"image/jpeg", jpeg}, {"image/png", png}}, PasteImage, ClipboardContent{Image: png, ImageType: "image/png"}},
		{"a uri list keeps its order", []x11Offer{{"UTF8_STRING", []byte("/tmp/b c")}, {"text/uri-list", []byte(uriList)}}, PasteOrdinary, ClipboardContent{Files: []string{"/tmp/b c", "/tmp/a", "/tmp/한"}}},
		{"gnome copied files keep their order", []x11Offer{{"x-special/gnome-copied-files", []byte("cut\nfile:///home/u/x.txt\nfile:///home/u/%C3%A9.png")}}, PasteOrdinary, ClipboardContent{Files: []string{"/home/u/x.txt", "/home/u/é.png"}}},
		{"image paste of files pastes the files", []x11Offer{{"UTF8_STRING", []byte("/tmp/a")}, {"text/uri-list", []byte("file:///tmp/a\r\n")}}, PasteImage, ClipboardContent{Files: []string{"/tmp/a"}}},
	}
	for _, c := range cases {
		startX11Owner(t, display, x11OwnerPolicy{}, c.offers...)
		got, err := readX11Clipboard(t, clipboard, c.kind)
		if err != nil {
			t.Fatalf("%s: %v", c.name, err)
		}
		if !sameClipboardContent(got, c.want) {
			t.Fatalf("%s: pasted %+q, want %+q", c.name, got, c.want)
		}
	}
}

func TestX11ClipboardRejectsUndecodableOffers(t *testing.T) {
	display := startX11Server(t, withoutTCP)
	clipboard := openX11ClipboardForTest(t, display)
	cases := []struct {
		offer x11Offer
		want  error
	}{
		{x11Offer{"UTF8_STRING", []byte("ok\xff\xfe")}, inputtext.InvalidEncoding},
		{x11Offer{"text/plain;charset=utf-8", []byte("\xed\xa0\x80")}, inputtext.InvalidEncoding},
		{x11Offer{"UTF8_STRING", []byte("a\x00b")}, inputtext.DisallowedControl},
		{x11Offer{"x-special/gnome-copied-files", []byte("move\nfile:///tmp/a")}, ErrUnsupportedPaste},
		{x11Offer{"text/uri-list", []byte("file:///tmp/a\r\nhttps://example.org/b\r\n")}, ErrNotLocalFile},
	}
	for _, c := range cases {
		startX11Owner(t, display, x11OwnerPolicy{}, c.offer)
		got, err := readX11Clipboard(t, clipboard, PasteOrdinary)
		if !errors.Is(err, c.want) || !sameClipboardContent(got, ClipboardContent{}) {
			t.Fatalf("%s %q pasted %+q with %v, want only %v", c.offer.target, c.offer.data, got, err, c.want)
		}
	}
}

func TestX11ClipboardRefusesPasteWithoutASupportedOffer(t *testing.T) {
	display := startX11Server(t, withoutTCP)
	clipboard := openX11ClipboardForTest(t, display)
	if _, err := readX11Clipboard(t, clipboard, PasteOrdinary); !errors.Is(err, ErrUnsupportedPaste) {
		t.Fatalf("a clipboard without an owner pasted with %v, want ErrUnsupportedPaste", err)
	}
	startX11Owner(t, display, x11OwnerPolicy{}, x11Offer{"STRING", []byte("latin")}, x11Offer{"COMPOUND_TEXT", []byte("text")})
	if _, err := readX11Clipboard(t, clipboard, PasteOrdinary); !errors.Is(err, ErrUnsupportedPaste) {
		t.Fatalf("an owner of only unsupported targets pasted with %v, want ErrUnsupportedPaste", err)
	}
	startX11Owner(t, display, x11OwnerPolicy{refuse: true}, x11Offer{"UTF8_STRING", []byte("refused")})
	if _, err := readX11Clipboard(t, clipboard, PasteOrdinary); !errors.Is(err, ErrUnsupportedPaste) {
		t.Fatalf("an owner refusing the conversion pasted with %v, want ErrUnsupportedPaste", err)
	}
}

func TestX11ClipboardReadsIncrementalTransfers(t *testing.T) {
	display := startX11Server(t, withoutTCP)
	clipboard := openX11ClipboardForTest(t, display)
	text := strings.Repeat("한글 e\u0301 \U0001F600\r\n", 40000)
	cases := []struct {
		offer x11Offer
		kind  PasteKind
		want  ClipboardContent
	}{
		{x11Offer{"UTF8_STRING", []byte(text)}, PasteOrdinary, ClipboardContent{Text: text}},
		{x11Offer{"image/png", x11Pattern(3 << 19)}, PasteImage, ClipboardContent{Image: x11Pattern(3 << 19), ImageType: "image/png"}},
	}
	for _, c := range cases {
		owner := startX11Owner(t, display, x11OwnerPolicy{}, c.offer)
		if len(c.offer.data) <= 4*owner.conn.maxRequestUnits {
			t.Fatalf("the %d-byte fixture fits in one %d-byte request", len(c.offer.data), 4*owner.conn.maxRequestUnits)
		}
		got, err := readX11Clipboard(t, clipboard, c.kind)
		if err != nil {
			t.Fatalf("%s: %v", c.offer.target, err)
		}
		if !sameClipboardContent(got, c.want) {
			t.Fatalf("%s: the incremental transfer pasted %d text bytes and %d image bytes, want the %d offered bytes", c.offer.target, len(got.Text), len(got.Image), len(c.offer.data))
		}
		if chunks := owner.chunks.Load(); chunks < 2 {
			t.Fatalf("%s: the owner sent %d INCR chunks, want an incremental transfer", c.offer.target, chunks)
		}
	}
}

func TestX11ClipboardRefusesTransfersPast32MiB(t *testing.T) {
	display := startX11Server(t, withoutTCP)
	clipboard := openX11ClipboardForTest(t, display)
	limit := x11Pattern(transferLimit)
	owner := startX11Owner(t, display, x11OwnerPolicy{}, x11Offer{"image/png", limit})
	got, err := readX11Clipboard(t, clipboard, PasteImage)
	if err != nil || !bytes.Equal(got.Image, limit) {
		t.Fatalf("a transfer of exactly 32 MiB pasted %d bytes with %v", len(got.Image), err)
	}
	chunk := owner.chunkSize()
	cases := []struct {
		name   string
		policy x11OwnerPolicy
		size   int
		chunks int64
	}{
		{"an announced incremental size past the bound", x11OwnerPolicy{}, transferLimit + 1, 0},
		{"incremental chunks past the bound", x11OwnerPolicy{hideSize: true}, transferLimit + 3*chunk, transferLimit/int64(chunk) + 1},
		{"one property past the bound", x11OwnerPolicy{storeWhole: true}, transferLimit + 1, 0},
	}
	for _, c := range cases {
		refusing := startX11Owner(t, display, c.policy, x11Offer{"image/png", x11Pattern(c.size)})
		refused, err := readX11Clipboard(t, clipboard, PasteImage)
		if !errors.Is(err, ErrTransferTooLarge) || refused.Image != nil {
			t.Fatalf("%s: pasted %d bytes with %v, want ErrTransferTooLarge", c.name, len(refused.Image), err)
		}
		if chunks := refusing.chunks.Load(); chunks != c.chunks {
			t.Fatalf("%s: the owner sent %d chunks, want %d before the refusal", c.name, chunks, c.chunks)
		}
	}
}

func TestX11ClipboardHonorsCancellationWhileTheOwnerStalls(t *testing.T) {
	display := startX11Server(t, withoutTCP)
	clipboard := openX11ClipboardForTest(t, display)
	owner := startX11Owner(t, display, x11OwnerPolicy{stall: true}, x11Offer{"UTF8_STRING", []byte("late")})
	ctx, cancel := context.WithCancel(t.Context())
	go func() {
		<-owner.requests
		cancel()
	}()
	if _, err := clipboard.ReadClipboard(ctx, PasteOrdinary); !errors.Is(err, context.Canceled) {
		t.Fatalf("a cancelled paste ended with %v, want context.Canceled", err)
	}
	deadline, stop := context.WithTimeout(t.Context(), 300*time.Millisecond)
	defer stop()
	if _, err := clipboard.ReadClipboard(deadline, PasteOrdinary); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("a paste past its deadline ended with %v, want context.DeadlineExceeded", err)
	}
	startX11Owner(t, display, x11OwnerPolicy{}, x11Offer{"UTF8_STRING", []byte("after")})
	got, err := readX11Clipboard(t, clipboard, PasteOrdinary)
	if err != nil || got.Text != "after" {
		t.Fatalf("the paste after the cancellations pasted %+q with %v, want \"after\"", got, err)
	}
}

func TestX11WindowOriginReportsTheRootPosition(t *testing.T) {
	display := startX11Server(t, withoutTCP)
	clipboard := openX11ClipboardForTest(t, display)
	probe := dialX11ForTest(t, display)
	framed := createX11TestWindow(t, probe, x11TestWindow{parent: probe.root, x: 37, y: 81, border: 5})
	cases := []struct {
		window uint32
		want   image.Point
	}{
		{framed, image.Pt(42, 86)},
		{createX11TestWindow(t, probe, x11TestWindow{parent: framed, x: 7, y: 9, border: 2}), image.Pt(51, 97)},
		{createX11TestWindow(t, probe, x11TestWindow{parent: probe.root, x: -20, y: -30}), image.Pt(-20, -30)},
	}
	for _, c := range cases {
		got, err := clipboard.WindowOrigin(c.window)
		if err != nil || got != c.want {
			t.Fatalf("window %#x origin is %v with %v, want %v", c.window, got, err, c.want)
		}
	}
	checkDestroyedWindowOrigin(t, clipboard, probe, framed)
}

func checkDestroyedWindowOrigin(t *testing.T, clipboard *X11Clipboard, probe *x11Conn, framed uint32) {
	t.Helper()
	gone := createX11TestWindow(t, probe, x11TestWindow{parent: probe.root})
	if err := probe.destroyWindow(gone); err != nil {
		t.Fatal(err)
	}
	if _, err := probe.internAtoms([]string{"PRIMARY"}); err != nil {
		t.Fatal(err)
	}
	_, err := clipboard.WindowOrigin(gone)
	var rejected *x11Error
	if !errors.As(err, &rejected) || rejected.code != x11TestWindowError || rejected.value != gone {
		t.Fatalf("the origin of a destroyed window returned %v, want a Window error naming %#x", err, gone)
	}
	if got, err := clipboard.WindowOrigin(framed); err != nil || got != image.Pt(42, 86) {
		t.Fatalf("after the Window error the origin is %v with %v", got, err)
	}
}

func TestX11WindowOriginUsesTheScreenOfTheDisplayName(t *testing.T) {
	display := startX11Server(t, func(number int) []string {
		return append(withoutTCP(number), "-screen", "0", "640x480x24", "-screen", "1", "320x240x24")
	})
	second := display + ".1"
	clipboard := openX11ClipboardForTest(t, second)
	probe := dialX11ForTest(t, second)
	first := dialX11ForTest(t, display)
	if probe.root == first.root {
		t.Fatalf("screens 0 and 1 share the root window %#x", probe.root)
	}
	window := createX11TestWindow(t, probe, x11TestWindow{parent: probe.root, x: 12, y: 34})
	if got, err := clipboard.WindowOrigin(window); err != nil || got != image.Pt(12, 34) {
		t.Fatalf("the origin on screen 1 is %v with %v, want (12,34)", got, err)
	}
	if _, err := openX11ClipboardForTest(t, display).WindowOrigin(window); err == nil {
		t.Fatal("screen 0 reported an origin for a window on screen 1")
	}
	if conn, err := dialX11(display + ".2"); err == nil {
		_ = conn.close()
		t.Fatal("a display name selected screen 2 of a two-screen server")
	}
}

func TestX11WindowOriginAnswersDuringAStalledPaste(t *testing.T) {
	display := startX11Server(t, withoutTCP)
	clipboard := openX11ClipboardForTest(t, display)
	probe := dialX11ForTest(t, display)
	window := createX11TestWindow(t, probe, x11TestWindow{parent: probe.root, x: 10, y: 20})
	owner := startX11Owner(t, display, x11OwnerPolicy{stall: true}, x11Offer{"UTF8_STRING", []byte("late")})
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()
	pasted := make(chan error, 1)
	go func() {
		_, err := clipboard.ReadClipboard(ctx, PasteOrdinary)
		pasted <- err
	}()
	<-owner.requests
	origin, err := clipboard.WindowOrigin(window)
	select {
	case ended := <-pasted:
		t.Fatalf("the origin waited for the stalled paste to end with %v", ended)
	default:
	}
	cancel()
	if ended := <-pasted; !errors.Is(ended, context.Canceled) {
		t.Fatalf("the stalled paste ended with %v, want context.Canceled", ended)
	}
	if err != nil || origin != image.Pt(10, 20) {
		t.Fatalf("the origin during the stalled paste is %v with %v, want (10,20)", origin, err)
	}
}

func TestX11ClipboardRefusesAMisalignedTargetList(t *testing.T) {
	var clipboard X11Clipboard
	if _, err := clipboard.choose(x11Property{kind: 1, format: 32, value: []byte{1, 2, 3}}, PasteOrdinary); !errors.Is(err, ErrUnsupportedPaste) {
		t.Fatalf("a target list of three bytes chose with %v, want ErrUnsupportedPaste", err)
	}
	if _, err := clipboard.choose(x11Property{kind: 1, format: 32}, PasteOrdinary); !errors.Is(err, ErrUnsupportedPaste) {
		t.Fatalf("an empty target list chose with %v, want ErrUnsupportedPaste", err)
	}
}

func TestX11ClipboardRefusesAnIncrementalTransferThatChangesType(t *testing.T) {
	display := startX11Server(t, withoutTCP)
	clipboard := openX11ClipboardForTest(t, display)
	data := x11Pattern(3 << 19)
	owner := startX11Owner(t, display, x11OwnerPolicy{corruptChunks: true}, x11Offer{"image/png", data})
	if len(data) <= 4*owner.conn.maxRequestUnits {
		t.Fatalf("the %d-byte fixture fits in one %d-byte request", len(data), 4*owner.conn.maxRequestUnits)
	}
	if _, err := readX11Clipboard(t, clipboard, PasteImage); !errors.Is(err, errX11Malformed) {
		t.Fatalf("a transfer that changes type midstream pasted with %v, want a malformed transfer refusal", err)
	}
	if chunks := owner.chunks.Load(); chunks < 2 {
		t.Fatalf("the owner sent %d INCR chunks, want an incremental transfer", chunks)
	}
}
