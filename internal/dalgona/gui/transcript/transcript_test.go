//go:build gui

package transcript

import (
	"context"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"image"
	"slices"
	"strconv"
	"strings"
	"sync"
	"testing"
	"testing/synctest"
	"time"

	"gioui.org/f32"
	"gioui.org/io/input"
	"gioui.org/io/key"
	"gioui.org/io/pointer"
	"gioui.org/io/semantic"
	"gioui.org/layout"
	"gioui.org/op"
	"gioui.org/text"
	"gioui.org/unit"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/access/accesstest"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const stream = "s1"

var sharedShaper = sync.OnceValues(platform.NewShaper)

type readCall struct {
	content string
	offset  uint64
}

type daemon struct {
	t         *testing.T
	mu        sync.Mutex
	owner     *state.Owner
	gen       client.Generation
	thread    string
	turns     []rpc.HistoryTurn
	contents  map[string][]byte
	images    map[string][]byte
	pending   []rpc.PendingRequest
	reads     []readCall
	failing   map[readCall]bool
	readGate  map[readCall]<-chan struct{}
	ordinal   uint64
	verbosity string
}

func (d *daemon) Connect(id client.EndpointID) error {
	d.mu.Lock()
	d.gen++
	link := client.Link{Endpoint: id, Generation: d.gen}
	d.mu.Unlock()
	d.owner.PhaseChanged(client.PhaseChange{Link: link, Phase: client.PhaseConnected, Native: rpc.NativeSession{StreamID: stream}})
	return nil
}

func (d *daemon) Disconnect(client.EndpointID) error { return nil }

func (d *daemon) Endpoints() []client.EndpointStatus {
	return []client.EndpointStatus{{ID: "local", Label: "Local"}}
}

func (d *daemon) Respond(context.Context, client.Link, string, any) error { return nil }

func (d *daemon) Attach(client.EndpointID) (func(), error) { return func() {}, nil }

func (d *daemon) CallOperation(context.Context, client.Link, rpc.Operation, any, any) error {
	return rpc.NativeKind(rpc.ErrorUnavailable)
}

func (d *daemon) Call(ctx context.Context, _ client.Link, method string, params, result any) error {
	raw, err := json.Marshal(params)
	if err != nil {
		return err
	}
	var in map[string]any
	if err = json.Unmarshal(raw, &in); err != nil {
		return err
	}
	if method == "dalgona/history/content/read" {
		if waitErr := d.awaitRead(ctx, in); waitErr != nil {
			return waitErr
		}
	}
	value, err := d.answer(method, in)
	if err != nil {
		return err
	}
	out, err := json.Marshal(value)
	if err != nil {
		return err
	}
	return json.Unmarshal(out, result)
}

func (d *daemon) awaitRead(ctx context.Context, in map[string]any) error {
	id, _ := in["contentId"].(string)
	offsetText, _ := in["offset"].(string)
	offset, err := strconv.ParseUint(cmpOr(offsetText, "0"), 10, 64)
	if err != nil {
		return err
	}
	d.mu.Lock()
	gate := d.readGate[readCall{content: id, offset: offset}]
	d.mu.Unlock()
	if gate == nil {
		return nil
	}
	select {
	case <-gate:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

func (d *daemon) answer(method string, in map[string]any) (any, error) {
	d.mu.Lock()
	defer d.mu.Unlock()
	switch method {
	case "dalgona/history/open":
		return rpc.HistorySnapshot{HistoryID: "h1", Thread: rpc.Thread{ID: d.thread, Name: "Demo"}, PathRevision: "p", StreamID: stream, CutOrdinal: "0", PendingRequests: append([]rpc.PendingRequest{}, d.pending...)}, nil
	case "dalgona/history/page":
		newest := slices.Clone(d.turns)
		slices.Reverse(newest)
		return rpc.HistoryPage{Turns: newest, PathRevision: "p"}, nil
	case "dalgona/history/content/read":
		return d.chunk(in)
	case "dalgona/request/details/read":
		in["contentId"] = in["requestId"]
		return d.chunk(in)
	case "dalgona/item/image/read":
		return d.image(in)
	case "dalgona/settings/read":
		return map[string]any{"settings": []map[string]any{{"key": "verbosity", "value": d.verbosity}}}, nil
	}
	return map[string]string{}, nil
}

func (d *daemon) image(in map[string]any) (any, error) {
	index, _ := in["imageIndex"].(float64)
	item, _ := in["itemId"].(string)
	data, ok := d.images[item+"#"+strconv.Itoa(int(index))]
	if !ok {
		return nil, rpc.NativeKind(rpc.ErrorNotFound)
	}
	offsetText, _ := in["offset"].(string)
	offset, err := strconv.ParseUint(cmpOr(offsetText, "0"), 10, 64)
	if err != nil {
		return nil, err
	}
	d.reads = append(d.reads, readCall{content: "image:" + item, offset: offset})
	end := min(offset+64<<10, uint64(len(data)))
	sum := sha256.Sum256(data)
	return map[string]string{
		"mimeType": "image/png", "digest": hex.EncodeToString(sum[:]), "total": rpc.FormatDecimal(uint64(len(data))),
		"offset": rpc.FormatDecimal(offset), "next": rpc.FormatDecimal(end), "dataBase64": base64.StdEncoding.EncodeToString(data[offset:end]),
	}, nil
}

func (d *daemon) chunk(in map[string]any) (any, error) {
	id, _ := in["contentId"].(string)
	offsetText, _ := in["offset"].(string)
	offset, err := strconv.ParseUint(cmpOr(offsetText, "0"), 10, 64)
	if err != nil {
		return nil, err
	}
	limit := uint64(256 << 10)
	if l, ok := in["limit"].(float64); ok && l > 0 {
		limit = uint64(l)
	}
	call := readCall{content: id, offset: offset}
	d.reads = append(d.reads, call)
	if d.failing[call] {
		delete(d.failing, call)
		return nil, rpc.NativeKind(rpc.ErrorUnavailable)
	}
	data, ok := d.contents[id]
	if !ok {
		return nil, rpc.NativeKind(rpc.ErrorNotFound)
	}
	end := min(offset+limit, uint64(len(data)))
	sum := sha256.Sum256(data)
	return rpc.ContentChunk{
		DataBase64: base64.StdEncoding.EncodeToString(data[offset:end]), Offset: rpc.FormatDecimal(offset),
		Next: rpc.FormatDecimal(end), Total: rpc.FormatDecimal(uint64(len(data))), Digest: hex.EncodeToString(sum[:]),
	}, nil
}

func (d *daemon) ref(id, field, encoding string, data []byte) rpc.ContentReference {
	d.contents[id] = data
	sum := sha256.Sum256(data)
	return rpc.ContentReference{Source: rpc.ContentSource{Kind: "snapshot", HistoryID: "h1"}, ContentID: id, Field: field, Encoding: encoding, Total: rpc.FormatDecimal(uint64(len(data))), Digest: hex.EncodeToString(sum[:])}
}

type fieldData struct {
	name string
	data string
	json bool
}

func (d *daemon) header(id, kind string, meta map[string]any, fields ...fieldData) rpc.ItemHeader {
	metadata := map[string]jsontext.Value{}
	for name, value := range meta {
		raw, err := json.Marshal(value)
		if err != nil {
			d.t.Fatal(err)
		}
		metadata[name] = raw
	}
	h := rpc.ItemHeader{ID: id, Type: kind, ItemRevision: id + "-" + kind, Metadata: metadata, Contents: []rpc.ContentReference{}}
	for _, f := range fields {
		encoding := "utf8"
		if f.json {
			encoding = "json"
		}
		h.Contents = append(h.Contents, d.ref(id+"."+f.name+"."+strconv.Itoa(len(d.contents)), f.name, encoding, []byte(f.data)))
	}
	return h
}

func frozenTurn(id, status string, items ...rpc.ItemHeader) rpc.HistoryTurn {
	return rpc.HistoryTurn{TurnID: id, TurnRevision: "r-" + id, Status: status, FirstItemIndex: 0, NextItemIndex: len(items), ItemsComplete: true, Items: items}
}

func (d *daemon) event(thread, method string, params any) {
	d.mu.Lock()
	d.ordinal++
	ordinal := d.ordinal
	link := client.Link{Endpoint: "local", Generation: d.gen}
	d.mu.Unlock()
	raw, err := json.Marshal(params)
	if err != nil {
		d.t.Fatal(err)
	}
	envelope, err := json.Marshal(rpc.EventParams{StreamID: stream, FirstOrdinal: rpc.FormatDecimal(ordinal), LastOrdinal: rpc.FormatDecimal(ordinal), ThreadID: thread, Method: method, Params: raw})
	if err != nil {
		d.t.Fatal(err)
	}
	d.owner.Notified(link, rpc.MethodEvent, envelope)
}

func (d *daemon) readsOf(content string) []uint64 {
	d.mu.Lock()
	defer d.mu.Unlock()
	var out []uint64
	for _, r := range d.reads {
		if strings.HasPrefix(r.content, content) {
			out = append(out, r.offset)
		}
	}
	return out
}

type harness struct {
	t         *testing.T
	d         *daemon
	tr        *Transcript
	router    *input.Router
	shaper    *text.Shaper
	now       time.Time
	key       state.ThreadKey
	size      image.Point
	routes    []shell.Route
	navigated []state.ThreadKey
	announced []platform.Announcement
	nodes     []input.SemanticNode
	ax        *accesstest.Frame
	modal     bool
	wait      func()
}

func withTranscript(t *testing.T, turns func(d *daemon) []rpc.HistoryTurn, body func(t *testing.T, h *harness)) {
	withTranscriptAt(t, "", turns, body)
}

func withTranscriptAt(t *testing.T, verbosity string, turns func(d *daemon) []rpc.HistoryTurn, body func(t *testing.T, h *harness)) {
	withTranscriptDeps(t, verbosity, Deps{}, turns, body)
}

type setup struct {
	verbosity string
	deps      Deps
	wait      func()
}

func withTranscriptDeps(t *testing.T, verbosity string, deps Deps, turns func(d *daemon) []rpc.HistoryTurn, body func(t *testing.T, h *harness)) {
	synctest.Test(t, func(t *testing.T) {
		runTranscript(t, setup{verbosity: verbosity, deps: deps, wait: synctest.Wait}, turns, body)
	})
}

func withTranscriptLive(t *testing.T, deps Deps, turns func(d *daemon) []rpc.HistoryTurn, body func(t *testing.T, h *harness)) {
	runTranscript(t, setup{deps: deps, wait: func() { time.Sleep(5 * time.Millisecond) }}, turns, body)
}

func runTranscript(t *testing.T, s setup, turns func(d *daemon) []rpc.HistoryTurn, body func(t *testing.T, h *harness)) {
	shaper, err := sharedShaper()
	if err != nil {
		t.Fatal(err)
	}
	d := &daemon{t: t, thread: "t1", contents: map[string][]byte{}, images: map[string][]byte{}, failing: map[readCall]bool{}, verbosity: s.verbosity}
	d.owner = state.New(func() {}, t.TempDir())
	d.turns = turns(d)
	ctx, cancel := context.WithCancel(t.Context())
	done := make(chan error, 1)
	go func() { done <- d.owner.Run(ctx, d) }()
	h := &harness{t: t, d: d, tr: New(s.deps), router: new(input.Router), shaper: shaper, now: time.Unix(1_800_000_000, 0), key: state.ThreadKey{Endpoint: "local", Thread: "t1"}, size: image.Pt(800, 600), wait: s.wait}
	d.owner.Submit(state.Connect{Endpoint: "local"})
	h.wait()
	d.owner.Submit(state.OpenView{Window: "w1", Thread: h.key})
	h.settle()
	body(t, h)
	cancel()
	if err := <-done; err != nil {
		t.Fatal(err)
	}
}

func (h *harness) frame() {
	h.t.Helper()
	h.wait()
	h.now = h.now.Add(16 * time.Millisecond)
	ops := new(op.Ops)
	gtx := layout.Context{Ops: ops, Now: h.now, Metric: unit.Metric{PxPerDp: 1, PxPerSp: 1}, Constraints: layout.Exact(h.size), Source: h.router.Source()}
	gtx = h.ax.Begin(gtx)
	env := shell.Env{
		Theme: theme.Night, Shaper: h.shaper, View: h.d.owner.View(), Submit: h.d.owner.Submit, Window: "w1", Thread: h.key, Modal: h.modal,
		Navigate: func(k state.ThreadKey) { h.navigated = append(h.navigated, k) },
		Open:     func(r shell.Route) { h.routes = append(h.routes, r) },
		Close:    func() {},
		Announce: func(a platform.Announcement) { h.announced = append(h.announced, a) },
	}
	h.tr.Layout(gtx, env)
	h.router.Frame(ops)
	h.ax.End(h.t, gtx)
	h.nodes = h.router.AppendSemantics(nil)
}

func (h *harness) settle() {
	h.t.Helper()
	for range 12 {
		h.frame()
		time.Sleep(60 * time.Millisecond)
	}
}

func (h *harness) labels() []string {
	out := make([]string, 0, len(h.nodes))
	for _, n := range h.nodes {
		if n.Desc.Label != "" {
			out = append(out, n.Desc.Label)
		}
	}
	return out
}

func (h *harness) has(label string) bool {
	for _, l := range h.labels() {
		if l == label {
			return true
		}
	}
	return false
}

func (h *harness) expect(labels ...string) {
	h.t.Helper()
	for _, l := range labels {
		if !h.has(l) {
			h.t.Fatalf("no semantic label %q among %q", l, h.labels())
		}
	}
}

func (h *harness) button(label string) (input.SemanticNode, bool) {
	for _, n := range h.nodes {
		if n.Desc.Label == label && n.Desc.Class == semantic.Button {
			return n, true
		}
	}
	return input.SemanticNode{}, false
}

func (h *harness) click(label string) {
	h.t.Helper()
	n, ok := h.button(label)
	if !ok {
		h.t.Fatalf("no button %q among %q", label, h.labels())
	}
	at := f32.Pt(float32(n.Desc.Bounds.Min.X+n.Desc.Bounds.Max.X)/2, float32(n.Desc.Bounds.Min.Y+n.Desc.Bounds.Max.Y)/2)
	h.router.Queue(
		pointer.Event{Kind: pointer.Press, Source: pointer.Mouse, Buttons: pointer.ButtonPrimary, Position: at, Time: time.Duration(h.now.UnixMilli()) * time.Millisecond},
		pointer.Event{Kind: pointer.Release, Source: pointer.Mouse, Position: at, Time: time.Duration(h.now.UnixMilli()) * time.Millisecond},
	)
	h.frame()
	h.frame()
}

func (h *harness) keyPress(name key.Name, mods key.Modifiers) {
	h.router.Queue(key.Event{Name: name, Modifiers: mods, State: key.Press})
	h.frame()
}

func (h *harness) scroll(dy float32) {
	h.router.Queue(pointer.Event{Kind: pointer.Scroll, Source: pointer.Mouse, Position: f32.Pt(400, 300), Scroll: f32.Pt(0, dy)})
	h.frame()
}

func commandTurn(d *daemon) []rpc.HistoryTurn {
	var out strings.Builder
	for i := 1; i <= 25; i++ {
		out.WriteString("line " + strconv.Itoa(i) + "\n")
	}
	return []rpc.HistoryTurn{
		frozenTurn("u1", rpc.StatusCompleted,
			d.header("i1", rpc.KindUser, map[string]any{
				"keywords": []map[string]any{{"word": "build", "byteRange": map[string]int{"start": 0, "end": 5}}},
				"content":  []map[string]any{{"type": "image", "url": "dalgona-blob:x", "mime": "image/png", "bytes": 2048}},
			}, fieldData{name: "text", data: "build it"}),
			d.header("i2", rpc.KindAgent, nil, fieldData{name: "text", data: "# Plan\n\nAll **done**."}),
			d.header("i3", rpc.KindCommand, map[string]any{"status": rpc.StatusCompleted, "exitCode": 2, "durationMs": 1500}, fieldData{name: "command", data: "make test"}, fieldData{name: "aggregatedOutput", data: out.String()}),
		),
		frozenTurn("u2", "interrupted", d.header("i1", rpc.KindAgent, nil, fieldData{name: "text", data: "second turn reply"})),
	}
}

func TestTranscriptRendersEachRowKindFromPagedContent(t *testing.T) {
	withTranscript(t, commandTurn, func(t *testing.T, h *harness) {
		h.expect("You", "build it", "Keywords: build", "Image 1 · image/png · 2.0 KiB", "Rewind to here",
			"dalgona", "Plan", "All done.", "$ make test", "exit 2 · 1s", "line 25", "Showing the last 10 of 25 lines.",
			"Open full text", "second turn reply", outcomeStopped)
		if h.has("line 15") {
			t.Fatal("the medium command tail shows more than its last ten lines")
		}
		for _, l := range h.labels() {
			if l == "# Plan" || strings.Contains(l, "**") {
				t.Fatalf("assistant Markdown leaked as literal source %q", l)
			}
		}
		if reads := h.d.readsOf("i3.aggregatedOutput"); len(reads) != 1 || reads[0] != 0 {
			t.Fatalf("command output read at offsets %v, want one page from 0", reads)
		}
	})
}

func TestEqualItemIdentifiersInDifferentTurnsStayDistinctRows(t *testing.T) {
	withTranscript(t, commandTurn, func(t *testing.T, h *harness) {
		p := h.tr.panes[h.key]
		first, ok1 := p.scope.Index(identity(h.key, "u1", "i1"))
		second, ok2 := p.scope.Index(identity(h.key, "u2", "i1"))
		if !ok1 || !ok2 || first == second {
			t.Fatalf("turn u1 item i1 at %d (%v), turn u2 item i1 at %d (%v)", first, ok1, second, ok2)
		}
		other := identity(state.ThreadKey{Endpoint: "remote", Thread: "t1"}, "u1", "i1")
		if _, ok := p.scope.Index(other); ok || other == identity(h.key, "u1", "i1") {
			t.Fatal("an equal thread on another endpoint shares the row identity")
		}
	})
}

func TestRewindAndNewThreadRouteToTheirPanels(t *testing.T) {
	failed := func(d *daemon) []rpc.HistoryTurn {
		turns := commandTurn(d)
		turns[1].Status = rpc.StatusFailed
		turns[1].Error = &rpc.TurnError{Message: "model overloaded"}
		return turns
	}
	withTranscript(t, failed, func(t *testing.T, h *harness) {
		h.expect("The turn failed: model overloaded. Send your message again to retry, or start a new thread.")
		h.click("New thread")
		h.router.Source().Execute(key.FocusCmd{Tag: h.tr.FocusTag()})
		h.keyPress(key.NameHome, 0)
		h.frame()
		h.click("Rewind to here")
		want := []shell.Route{{Panel: "new", Thread: h.key}, {Panel: "revert", Thread: h.key, Turn: "u1"}}
		if len(h.routes) != 2 || h.routes[0] != want[0] || h.routes[1] != want[1] {
			t.Fatalf("routes %+v, want %+v", h.routes, want)
		}
	})
}

func manyTurns(d *daemon) []rpc.HistoryTurn {
	var turns []rpc.HistoryTurn
	for i := range 18 {
		id := "u" + strconv.Itoa(i)
		turns = append(turns, frozenTurn(id, rpc.StatusCompleted, d.header("a"+strconv.Itoa(i), rpc.KindAgent, nil, fieldData{name: "text", data: "reply " + strconv.Itoa(i) + "\n\nsecond paragraph"})))
	}
	return turns
}

func TestJumpToLatestAppearsWhenDetachedCountsNewItemsAndRestoresFollowing(t *testing.T) {
	withTranscript(t, manyTurns, func(t *testing.T, h *harness) {
		if _, ok := h.button("Jump to latest"); ok {
			t.Fatal("a following transcript offers Jump to latest")
		}
		h.expect("reply 17")
		h.scroll(-20)
		if _, ok := h.button("Jump to latest"); ok {
			t.Fatal("scrolling 20 dp, inside the follow distance, detached following")
		}
		h.scroll(-400)
		if _, ok := h.button("Jump to latest"); !ok {
			t.Fatalf("detached transcript shows no Jump to latest among %q", h.labels())
		}
		p := h.tr.panes[h.key]
		top := p.vp.Offset()
		h.d.event("t1", "turn/started", map[string]any{"turn": map[string]any{"id": "u30", "status": "inProgress"}})
		h.d.event("t1", "item/started", map[string]any{"turnId": "u30", "item": h.d.header("n1", rpc.KindAgent, nil)})
		h.d.event("t1", "item/agentMessage/delta", map[string]any{"turnId": "u30", "itemId": "n1", "delta": "fresh output"})
		h.settle()
		if p.vp.Offset() != top {
			t.Fatalf("new output moved the detached reader from %v to %v", top, p.vp.Offset())
		}
		checkJumpCountAndFocus(t, h, p)
	})
}

func checkJumpCountAndFocus(t *testing.T, h *harness, p *pane) {
	t.Helper()
	label := ""
	for _, l := range h.labels() {
		if strings.HasPrefix(l, "Jump to latest · ") {
			label = l
		}
	}
	if !strings.HasSuffix(label, " new") || label == "Jump to latest · 0 new" {
		t.Fatalf("jump action reads %q, want an appended-item count", label)
	}
	h.click(label)
	if !p.vp.Following() || p.vp.Unseen() != 0 {
		t.Fatalf("jump left following %v unseen %d", p.vp.Following(), p.vp.Unseen())
	}
	h.frame()
	if !h.router.Source().Focused(&p.lastTag) {
		t.Fatal("jump to latest did not focus the last row")
	}
	h.expect("fresh output")
}

func spilled(d *daemon) []rpc.HistoryTurn {
	big := strings.Repeat("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcde\n", 600<<10/64)
	return []rpc.HistoryTurn{frozenTurn("u1", rpc.StatusCompleted,
		d.header("c1", rpc.KindCommand, map[string]any{"status": rpc.StatusCompleted, "exitCode": 0}, fieldData{name: "command", data: "seq 1 big"}, fieldData{name: "aggregatedOutput", data: big}),
	)}
}

func (h *harness) settleUntil(label string) {
	h.t.Helper()
	for range 8 {
		if h.has(label) {
			return
		}
		h.settle()
	}
	h.expect(label)
}

func TestViewerPagesSpilledOutputAndRetriesTheFailedCursor(t *testing.T) {
	withTranscript(t, spilled, func(t *testing.T, h *harness) {
		total := uint64(600<<10) / 64 * 64
		h.settleUntil("Showing the last 10 of 9600 lines.")
		reads := slices.Sorted(slices.Values(h.d.readsOf("c1.aggregatedOutput")))
		if want := []uint64{0, 256 << 10, total - tailKeep, 512 << 10}; !slices.Equal(reads, slices.Sorted(slices.Values(want))) {
			t.Fatalf("the transcript row read %v, want the tail page at %d and one counting pass %v", reads, total-tailKeep, want)
		}
		h.d.mu.Lock()
		h.d.reads = nil
		h.d.failing[readCall{content: h.d.turns[0].Items[0].Contents[1].ContentID, offset: 256 << 10}] = true
		h.d.mu.Unlock()
		h.keyPress("O", key.ModShortcut)
		h.settle()
		h.expect("Full text", "Close", "Copy all", "Export")
		output := h.tr.review.fields[1]
		if got := output.state(); got != "Showing 256.0 KiB of 600.0 KiB." {
			t.Fatalf("viewer output state %q", got)
		}
		if reads := h.d.readsOf("c1.aggregatedOutput"); !slices.Equal(reads, []uint64{0}) {
			t.Fatalf("opening the viewer read %v, want exactly the first page", reads)
		}
		checkSpilledOutputRetry(t, h, output)
		h.keyPress(key.NameEscape, 0)
		if h.has("Full text") {
			t.Fatal("Escape left the viewer open")
		}
	})
}

func checkSpilledOutputRetry(t *testing.T, h *harness, output *viewField) {
	t.Helper()
	output.pull = true
	h.settle()
	if output.err == nil || !strings.HasPrefix(output.state(), "Loading stopped at byte 262144") || output.next != 256<<10 {
		t.Fatalf("a failed page left state %q at %d", output.state(), output.next)
	}
	fieldActions(func(_ *widget.Clickable, name string, _ bool, act func()) {
		if name == "Retry" {
			act()
		}
	}, output)
	h.settle()
	reads := h.d.readsOf("c1.aggregatedOutput")
	if len(reads) < 3 || !slices.Equal(reads[1:3], []uint64{256 << 10, 256 << 10}) {
		t.Fatalf("retry read offsets %v, want the failed cursor 262144 again", reads)
	}
	if got := output.state(); got != "Showing 512.0 KiB of 600.0 KiB." || !strings.HasPrefix(output.text(), "0123456789abcdef") {
		t.Fatalf("after retry the viewer state is %q", got)
	}
}

func TestViewerDistinguishesMissingFromEmptyOutput(t *testing.T) {
	empty := func(d *daemon) []rpc.HistoryTurn {
		return []rpc.HistoryTurn{frozenTurn("u1", rpc.StatusCompleted,
			d.header("c1", rpc.KindCommand, map[string]any{"status": rpc.StatusCompleted}, fieldData{name: "command", data: "true"}, fieldData{name: "aggregatedOutput", data: ""}),
			d.header("c2", rpc.KindCommand, map[string]any{"status": rpc.StatusCompleted}, fieldData{name: "command", data: "silent"}),
		)}
	}
	withTranscript(t, empty, func(t *testing.T, h *harness) {
		h.keyPress("O", key.ModShortcut)
		h.settle()
		h.expect("No content was recorded for this item.")
		h.keyPress(key.NameEscape, 0)
		p := h.tr.panes[h.key]
		h.tr.openReview(layout.Context{Ops: new(op.Ops), Source: h.router.Source()}, p.rows[identity(h.key, "u1", "c1")], &h.tr.focus)
		h.settle()
		h.expect("Empty output.")
	})
}

func TestLiveReviewLineUsesTheServerSummaryAndAnnouncesOnce(t *testing.T) {
	change := func(d *daemon) []rpc.HistoryTurn {
		changes := `[{"path":"a.go","kind":{"type":"update"},"diff":"@@ -1 +1 @@\n-old\n+new\n","additions":1,"deletions":1}]`
		return []rpc.HistoryTurn{frozenTurn("u1", rpc.StatusCompleted, d.header("f1", rpc.KindFileChange, map[string]any{"status": rpc.StatusCompleted}, fieldData{name: "changes", data: changes, json: true}))}
	}
	withTranscript(t, change, func(t *testing.T, h *harness) {
		h.expect("diff a.go · Changed · +1 −1", "-old", "+new", "Collapse file")
		if h.has("Edit looks risky: check the guard") {
			t.Fatal("history invented a live review verdict")
		}
		h.d.event("t1", "dalgona/tool/judged", map[string]any{"threadId": "t1", "turnId": "u1", "itemId": "f1", "verdict": "flagged", "summary": "Edit looks risky: check the guard", "paths": []string{"a.go"}, "by": "judge"})
		h.settle()
		h.expect("Edit looks risky: check the guard")
		count := 0
		for _, a := range h.announced {
			if a.Text == "Edit looks risky: check the guard" {
				count++
			}
		}
		if count != 1 {
			t.Fatalf("the review notice was announced %d times", count)
		}
		checkReviewFileFold(t, h)
	})
}

func checkReviewFileFold(t *testing.T, h *harness) {
	t.Helper()
	h.click("Collapse file")
	h.settle()
	h.expect("diff a.go · Changed · +1 −1", "… 3 more lines folded", "Expand file")
	if h.has("+new") || h.has("-old") {
		t.Fatal("a folded file section still shows its lines")
	}
	p := h.tr.panes[h.key]
	doc := p.rows[identity(h.key, "u1", "f1")].doc
	if folds := doc.Folds(); len(folds) != 1 || !p.scope.Folded(folds[0].Key) {
		t.Fatalf("the scope does not hold the fold of %+v", folds)
	}
	h.click("Expand file")
	h.settle()
	h.expect("+new", "-old", "Collapse file")
}

func TestPlanChildrenAndAdvisorRows(t *testing.T) {
	rows := func(d *daemon) []rpc.HistoryTurn {
		return []rpc.HistoryTurn{frozenTurn("u1", rpc.StatusInProgress,
			d.header("adv", rpc.KindDynamic, map[string]any{"tool": "advisor", "status": rpc.StatusCompleted},
				fieldData{name: "arguments", data: `{"advisor":"sage","severity":"blocker"}`, json: true},
				fieldData{name: "contentItems", data: `[{"type":"inputText","text":"Stop: the migration drops data.\nRoll back first."}]`, json: true}),
		)}
	}
	withTranscript(t, rows, func(t *testing.T, h *harness) {
		h.d.event("t1", "turn/started", map[string]any{"turn": map[string]any{"id": "u1", "status": "inProgress"}})
		h.d.event("t1", "turn/plan/updated", map[string]any{"threadId": "t1", "turnId": "u1", "plan": []map[string]string{{"step": "Read code", "status": "completed"}, {"step": "Write tests", "status": "inProgress"}, {"step": "Ship", "status": "pending"}}})
		h.d.event("t1", "dalgona/thread/childSpawned", map[string]any{"threadId": "t1", "childId": "kid-7", "cwd": "/w"})
		h.settle()
		h.expect("advisor sage · blocker:", "Stop: the migration drops data.", "Roll back first.", "Done · Read code", "Working · Write tests", "Pending · Ship", "Subagents", "kid-7 · Ready", "Open")
		checkCurrentPlanStep(t, h.nodes)
		h.click("Open")
		if len(h.navigated) != 1 || h.navigated[0] != (state.ThreadKey{Endpoint: "local", Thread: "kid-7"}) {
			t.Fatalf("child Open navigated to %+v", h.navigated)
		}
	})
}

func checkCurrentPlanStep(t *testing.T, nodes []input.SemanticNode) {
	t.Helper()
	current := 0
	for _, n := range nodes {
		if !n.Desc.Selected || n.Desc.Description != "current step" {
			continue
		}
		current++
		if n.Desc.Label != "Working · Write tests" {
			t.Fatalf("current step is %q", n.Desc.Label)
		}
	}
	if current != 1 {
		t.Fatalf("%d steps expose the current-step state", current)
	}
}

func TestSelectionAcrossRowsCopiesSourceText(t *testing.T) {
	withTranscript(t, commandTurn, func(t *testing.T, h *harness) {
		h.router.Source().Execute(key.FocusCmd{Tag: h.tr.FocusTag()})
		h.keyPress(key.NameHome, 0)
		h.frame()
		first, okFirst := visibleLabelBounds(h, "build it")
		last, okLast := visibleLabelBounds(h, "All done.")
		if !okFirst || !okLast {
			t.Fatalf("rows not found among %q", h.labels())
		}
		from := f32.Pt(float32(first.Min.X)+1, float32(first.Min.Y+first.Max.Y)/2)
		to := f32.Pt(float32(last.Max.X)-1, float32(last.Min.Y+last.Max.Y)/2)
		h.router.Queue(
			pointer.Event{Kind: pointer.Press, Source: pointer.Mouse, Buttons: pointer.ButtonPrimary, Position: from},
			pointer.Event{Kind: pointer.Release, Source: pointer.Mouse, Position: from},
		)
		h.frame()
		h.router.Queue(
			pointer.Event{Kind: pointer.Press, Source: pointer.Mouse, Buttons: pointer.ButtonPrimary, Position: to, Modifiers: key.ModShift},
			pointer.Event{Kind: pointer.Release, Source: pointer.Mouse, Position: to, Modifiers: key.ModShift},
		)
		h.frame()
		h.keyPress("C", key.ModShortcut)
		mime, content, ok := h.router.WriteClipboard()
		if !ok || mime != "application/text" {
			t.Fatalf("copy wrote nothing (%q); transcript notice %q, selected %v", mime, h.tr.notice, h.tr.panes[h.key].sel.Selected)
		}
		got := string(content)
		if got != "build it\n\nPlan\n\nAll done." {
			t.Fatalf("copied %q", got)
		}
	})
}

func TestVerbositySettingSwitchesPresentation(t *testing.T) {
	cases := map[string]func(h *harness){
		"high": func(h *harness) {
			h.expect("line 1", "line 25")
			if h.has("Showing the last 10 of 25 lines.") {
				h.t.Fatal("high verbosity truncated a successful command")
			}
		},
		"low": func(h *harness) {
			h.expect("Ran 1 command", "You", "All done.")
			if h.has("$ make test") {
				h.t.Fatal("low verbosity showed a grouped command row")
			}
		},
	}
	for level, check := range cases {
		t.Run(level, func(t *testing.T) {
			withTranscriptAt(t, level, commandTurn, func(t *testing.T, h *harness) { check(h) })
		})
	}
}
