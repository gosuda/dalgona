//go:build gui

package transcript

import (
	"context"
	"image"
	"strings"
	"testing"
	"time"

	"gioui.org/layout"
	"gioui.org/op"
	"gioui.org/text"
	"gioui.org/unit"

	"github.com/gosuda/dalgona/internal/dalgona/gui/document"
	"github.com/gosuda/dalgona/internal/dalgona/gui/figure"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

func linkOf(t *testing.T, target string) document.Link {
	t.Helper()
	return document.Link{Destination: target}
}

func waitFor(t *testing.T, what string, cond func() bool) {
	t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for !cond() {
		if time.Now().After(deadline) {
			t.Fatalf("timed out waiting for %s", what)
		}
		time.Sleep(5 * time.Millisecond)
	}
}

func TestLinkOpenDoesNotBlockTheFrame(t *testing.T) {
	release := make(chan struct{})
	var opened []string
	tr := &Transcript{deps: Deps{OpenLink: func(target string) error {
		<-release
		opened = append(opened, target)
		return nil
	}}}
	tr.openLink(linkOf(t, "https://example.com/guide"))
	if len(opened) != 0 {
		t.Fatalf("openLink opened %v before the opener finished", opened)
	}
	close(release)
	waitFor(t, "the link notice", func() bool {
		tr.drainLinks()
		return tr.notice == "Opened https://example.com/guide"
	})
}

func TestStaleLinkResultIsDroppedAfterClose(t *testing.T) {
	release := make(chan struct{})
	tr := &Transcript{deps: Deps{OpenLink: func(target string) error {
		<-release
		return nil
	}}}
	tr.openLink(linkOf(t, "https://example.com/guide"))
	tr.Close()
	close(release)
	time.Sleep(50 * time.Millisecond)
	tr.drainLinks()
	if tr.notice != "" {
		t.Fatalf("closed window delivered notice %q", tr.notice)
	}
}

func TestCloseCancelsReviewJobsAndUnblocksFetch(t *testing.T) {
	r := &reviewer{}
	cancelled := false
	r.jobs = []*job{{cancel: func() { cancelled = true }}}
	got := make(chan pageResult, 1)
	go func() { got <- r.bridge.fetch(context.Background(), pageRequest{}) }()
	waitFor(t, "the fetch to register", func() bool {
		r.bridge.mu.Lock()
		defer r.bridge.mu.Unlock()
		return len(r.bridge.pending) == 1
	})
	r.cancelJobs()
	select {
	case res := <-got:
		if res.err == nil || res.err.Error() != errThreadClosed.Error() {
			t.Fatalf("fetch ended with %v, want the thread-closed error", res.err)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("fetch stayed blocked after cancelJobs")
	}
	if !cancelled || len(r.jobs) != 0 {
		t.Fatalf("cancelJobs left cancelled=%v with %d jobs", cancelled, len(r.jobs))
	}
}

func TestReviewCommitDropsStaleGenerations(t *testing.T) {
	r := &reviewer{gen: 2}
	applied := false
	if r.commit(1, func() { applied = true }) || applied {
		t.Fatal("a stale generation applied its result")
	}
	if !r.commit(2, func() { applied = true }) || !applied {
		t.Fatal("the current generation did not apply its result")
	}
}

func TestLoaderEvictsUsedBodiesOverBudget(t *testing.T) {
	l := newLoader()
	l.clock = 7
	bodies := map[bodyKey]*body{}
	for _, name := range []string{"a", "b"} {
		key := bodyKey{content: name}
		bodies[key] = &body{key: key, data: make([]byte, 20<<20), used: l.clock}
	}
	l.bodies = bodies
	l.resident = 40 << 20
	l.evict()
	if l.resident > residentBudget {
		t.Fatalf("resident source is %d bytes, want at most the %d-byte window budget", l.resident, residentBudget)
	}
}

func TestTailBodyPromotesToFullOnDemand(t *testing.T) {
	l := newLoader()
	b := &body{key: bodyKey{}, total: 300000, start: 300000 - 64000, next: 300000, data: make([]byte, 64000)}
	l.resident = len(b.data)
	l.promote(b)
	if b.start != 0 || b.next != 0 || !b.full || b.data != nil || l.resident != 0 {
		t.Fatalf("promote left start=%d next=%d full=%v data=%d resident=%d, want a fresh from-zero load",
			b.start, b.next, b.full, len(b.data), l.resident)
	}
}

func TestFigureSweepCancelsStaleJobsAndCapsEntries(t *testing.T) {
	sup := &figure.Supervisor{}
	tr := &Transcript{deps: Deps{Figures: &figure.Hub{Supervisor: sup}}, figures: map[document.Key]*figureState{}, figureClock: 100}
	for i := range figureStateLimit + 6 {
		tr.figures[document.Key{Hash: uint64(i + 1)}] = &figureState{seen: uint64(i + 1)}
	}
	stale := &figureState{seen: 1, job: 9}
	tr.figures[document.Key{Hash: 999}] = stale
	tr.sweepFigures()
	if len(tr.figures) > figureStateLimit {
		t.Fatalf("sweep kept %d figure states, want at most %d", len(tr.figures), figureStateLimit)
	}
	if stale.job != 0 {
		t.Fatal("sweep left the off-screen job running")
	}
}

func TestFigureOutcomeNeedsTheCurrentJob(t *testing.T) {
	cold := &figureState{}
	if !cold.adopts(figure.Outcome{Job: 5}) {
		t.Fatal("a fresh state refuses the cached outcome for its key")
	}
	running := &figureState{job: 7}
	if !running.adopts(figure.Outcome{Job: 7}) {
		t.Fatal("a running job refuses its own outcome")
	}
	if running.adopts(figure.Outcome{Job: 9}) {
		t.Fatal("a running job adopts another job's outcome")
	}
}

func testFrame(row *row) *frame {
	return &frame{
		gtx: layout.Context{
			Ops:         new(op.Ops),
			Metric:      unit.Metric{PxPerDp: 1, PxPerSp: 1},
			Constraints: layout.Exact(image.Pt(800, 600)),
		},
		env:    shell.Env{Shaper: text.NewShaper(text.NoSystemFonts())},
		row:    row,
		ref:    document.RowRef{Row: 0},
		column: 800,
	}
}

func testFigureRow(t *testing.T) *row {
	t.Helper()
	id, err := document.Committed("local", "thread-1", "turn-1", "item-1")
	if err != nil {
		t.Fatal(err)
	}
	doc, err := document.New(document.Header{Identity: id, Format: document.FormatMarkdown}, "text")
	if err != nil {
		t.Fatal(err)
	}
	return &row{id: id, doc: doc}
}

func TestFigureDropsCanvasWhenRenderInputsChange(t *testing.T) {
	sup, err := figure.NewSupervisor(figure.SupervisorConfig{Fonts: figureFonts()})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(sup.Close)
	hub := figure.NewHub(sup, nil)
	tr := &Transcript{deps: Deps{Figures: hub}}
	kind, ok := figure.ParseKind("mermaid")
	if !ok {
		t.Fatal("no mermaid figure kind registered")
	}
	source := "graph TD\nA-->B"
	f := testFrame(testFigureRow(t))
	s := &figureState{}
	tr.renderFigure(f, s, kind, source, false)
	if s.key == "" || s.job == 0 {
		t.Fatalf("initial render left key %q job %d, want a submitted job", s.key, s.job)
	}
	first := s.key
	tr.palette.Text = theme.Palette{}.Text
	tr.palette.Text.R ^= 0xff
	tr.renderFigure(f, s, kind, source, false)
	if s.key == first {
		t.Fatal("a palette change kept the stale figure layout")
	}
	if s.canvas != nil {
		t.Fatal("a palette change kept the stale figure canvas")
	}
}

func TestSaturatedRenderStaysWaitingInsteadOfFailing(t *testing.T) {
	sup, err := figure.NewSupervisor(figure.SupervisorConfig{Fonts: figureFonts()})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(sup.Close)
	big := []byte(strings.Repeat("value = 1\n", 1<<18))
	for range 8 {
		if _, _, err := sup.Highlight(figure.HighlightRequest{Document: "d", Revision: 1, Language: "go", Source: big, Visible: true}); err != nil {
			t.Fatalf("setup highlight: %v", err)
		}
	}
	hub := figure.NewHub(sup, nil)
	tr := &Transcript{deps: Deps{Figures: hub}}
	kind, ok := figure.ParseKind("mermaid")
	if !ok {
		t.Fatal("no mermaid figure kind registered")
	}
	s := &figureState{}
	tr.renderFigure(testFrame(testFigureRow(t)), s, kind, "graph TD\nA-->B", false)
	if s.err != nil {
		t.Fatalf("saturated render failed with %v, want a waiting retry", s.err)
	}
	if s.job != 0 {
		t.Fatalf("saturated render recorded job %d, want no job while waiting", s.job)
	}
}

func TestContentKeyTracksEveryRenderInput(t *testing.T) {
	sup := &figure.Supervisor{}
	kind, ok := figure.ParseKind("mermaid")
	if !ok {
		t.Fatal("no mermaid figure kind registered")
	}
	base := figure.Request{Kind: kind, Source: []byte("graph TD\nA-->B"), Scale: 1, Viewport: [2]float64{800, 800}}
	themeReq := base
	themeReq.Theme = figure.Theme{Text: figure.Color{R: 1, G: 2, B: 3, A: 255}}
	scaleReq := base
	scaleReq.Scale = 2
	if got := sup.ContentKey(base); got != sup.ContentKey(base) {
		t.Fatal("identical render inputs hash differently")
	}
	if sup.ContentKey(themeReq) == sup.ContentKey(base) {
		t.Fatal("a theme change kept the figure content key")
	}
	if sup.ContentKey(scaleReq) == sup.ContentKey(base) {
		t.Fatal("a scale change kept the figure content key")
	}
}

func TestRevealMatchPullsTheMatchWindow(t *testing.T) {
	r := &reviewer{}
	f := &viewField{hasRef: true, base: 1 << 20}
	r.fields = []*viewField{f}
	r.match = &matchSpot{field: 0, offset: 500}
	r.revealMatch(nil)
	if !f.pull || f.base != 0 {
		t.Fatalf("reveal left base=%d pull=%v, want the window repositioned to the match", f.base, f.pull)
	}
}

func TestFindCountsMatchesAcrossPageBoundariesOnce(t *testing.T) {
	pager := slicePager{pages: map[int]string{0: "aa", 2: "a"}}
	match, err := findField(context.Background(), pager, document.Ref{Length: 3}, "aa")
	if err != nil {
		t.Fatal(err)
	}
	if match.count != 1 || match.first != 0 {
		t.Fatalf("split match counted %+v, want one match at zero", match)
	}
	if _, err := findField(context.Background(), slicePager{}, document.Ref{Length: 4}, "aa"); err == nil {
		t.Fatal("an empty page before the end reported success")
	}
}

type slicePager struct {
	pages map[int]string
}

func (p slicePager) Page(_ context.Context, _ document.Ref, offset, _ int) (string, error) {
	return p.pages[offset], nil
}

func TestSaturatedHighlightStaysWaitingInsteadOfFailing(t *testing.T) {
	sup, err := figure.NewSupervisor(figure.SupervisorConfig{Fonts: figureFonts()})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(sup.Close)
	big := []byte(strings.Repeat("value = 1\n", 1<<18))
	for range 8 {
		if _, _, err := sup.Highlight(figure.HighlightRequest{Document: "d", Revision: 1, Language: "go", Source: big, Visible: true}); err != nil {
			t.Fatalf("setup highlight: %v", err)
		}
	}
	hub := figure.NewHub(sup, nil)
	tr := &Transcript{deps: Deps{Figures: hub}}
	r := testFigureRow(t)
	b := document.Block{Kind: document.KindCode, Language: "go", Fingerprint: 7, Projection: document.Projection{Text: "package main\n"}}
	if spans := tr.codeTokens(r, b); spans != nil {
		t.Fatalf("saturated highlight returned %d spans, want none while waiting", len(spans))
	}
	st := r.highlights[b.Key]
	if st == nil {
		t.Fatal("saturated highlight left no state to retry")
	}
	if st.job != 0 {
		t.Fatalf("saturated highlight recorded job %d, want no job while waiting", st.job)
	}
	if st.failed || tr.notice != "" {
		t.Fatalf("saturated highlight failed (%v, notice %q), want a waiting retry", st.failed, tr.notice)
	}
}
