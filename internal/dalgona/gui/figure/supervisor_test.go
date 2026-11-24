package figure

import (
	"errors"
	"fmt"
	"os"
	"os/exec"
	"strings"
	"testing"
	"time"
)

var testTheme = Theme{
	Canvas: Color{0xFF, 0xFC, 0xF0, 0xFF}, Surface: Color{0xE6, 0xE4, 0xD9, 0xFF}, Tone: Color{0xCE, 0xCD, 0xC3, 0xFF},
	Line: Color{0xB7, 0xB5, 0xAC, 0xFF}, Strong: Color{0x6F, 0x6E, 0x69, 0xFF}, Text: Color{0x10, 0x0F, 0x0F, 0xFF},
	Dim: Color{0x40, 0x3E, 0x3C, 0xFF}, Accent: Color{0x71, 0x32, 0x0D, 0xFF},
}

func newTestSupervisor(t *testing.T, args ...string) *Supervisor {
	t.Helper()
	cfg := SupervisorConfig{Fonts: testFonts()}
	if len(args) > 0 {
		cfg.Args = args
	}
	s, err := NewSupervisor(cfg)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(s.Close)
	return s
}

func figureRequestOf(kind Kind, source string) Request {
	return Request{Document: "doc", Revision: 1, Kind: kind, Source: []byte(source), Theme: testTheme, Scale: 1, Viewport: [2]float64{640, 480}, Visible: true}
}

func awaitOutcome(t *testing.T, s *Supervisor, job uint64, limit time.Duration) Outcome {
	t.Helper()
	timer := time.NewTimer(limit)
	defer timer.Stop()
	for {
		select {
		case o := <-s.Outcomes():
			if o.Job == job {
				return o
			}
		case <-timer.C:
			t.Fatalf("job %d produced no outcome within %s", job, limit)
		}
	}
}

func noncooperativeD2() string {
	var b strings.Builder
	for i := range GraphEdgeLimit {
		fmt.Fprintf(&b, "n%d -> n%d\n", (i*7919)%GraphNodeLimit, (i*104729+13)%GraphNodeLimit)
	}
	return b.String()
}

func TestSupervisorKillsDiagramAtDeadlineAndStaysResponsive(t *testing.T) {
	s := newTestSupervisor(t)
	awaitRendered(t, s, `<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"/>`)
	first := s.worker
	started := time.Now()
	slow, _, err := s.Render(figureRequestOf(KindD2, noncooperativeD2()))
	if err != nil {
		t.Fatal(err)
	}
	requireResponsiveUntil(t, s, started.Add(RenderDeadline))
	o := awaitOutcome(t, s, slow, RenderDeadline+TerminationGrace+5*time.Second)
	elapsed := time.Since(started)
	if o.Err == nil || o.Err.Code != FailureTimeout {
		t.Fatalf("slow figure outcome = %+v, want a timeout failure", o.Err)
	}
	if elapsed > RenderDeadline+TerminationGrace+time.Second {
		t.Fatalf("timeout arrived after %s; the deadline plus grace is %s", elapsed, RenderDeadline+TerminationGrace)
	}
	requireKilled(t, first)
	if next := awaitRendered(t, s, `<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"/>`); next.List == nil || next.List.Width != 8 {
		t.Fatalf("figure after restart = %+v, want an 8 pixel figure", next)
	}
	if got := s.Launches(); got != 2 {
		t.Fatalf("renderer launches = %d, want one restart after the timeout", got)
	}
}

func awaitRendered(t *testing.T, s *Supervisor, source string) Outcome {
	t.Helper()
	id, _, err := s.Render(figureRequestOf(KindSVG, source))
	if err != nil {
		t.Fatal(err)
	}
	o := awaitOutcome(t, s, id, 3*helloTimeout)
	if o.Err != nil {
		t.Fatalf("figure %s failed: %v", source, o.Err)
	}
	return o
}

func requireResponsiveUntil(t *testing.T, s *Supervisor, deadline time.Time) {
	t.Helper()
	for time.Now().Before(deadline) {
		probe := time.Now()
		if s.Paused() {
			t.Fatal("supervisor paused after one slow figure")
		}
		if d := time.Since(probe); d > 50*time.Millisecond {
			t.Fatalf("supervisor call blocked for %s while a figure rendered", d)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

func requireKilled(t *testing.T, w *workerProcess) {
	t.Helper()
	select {
	case <-w.exited:
	case <-time.After(time.Second):
		t.Fatal("timed-out renderer process was not reaped")
	}
	if w.cmd.ProcessState == nil || w.cmd.ProcessState.Success() {
		t.Fatalf("timed-out renderer state = %v, want a killed process", w.cmd.ProcessState)
	}
}

func TestSupervisorRejectsMismatchedNonce(t *testing.T) {
	s := newTestSupervisor(t, testChildWrongNonce)
	id, _, err := s.Render(figureRequestOf(KindSVG, `<svg/>`))
	if err != nil {
		t.Fatal(err)
	}
	o := awaitOutcome(t, s, id, helloTimeout)
	if o.Err == nil || o.Err.Code != FailureProtocol || !strings.Contains(o.Err.Message, "nonce") {
		t.Fatalf("outcome = %+v, want a nonce protocol failure", o.Err)
	}
}

func TestSupervisorRejectsOversizedReplyBeforeAllocation(t *testing.T) {
	s := newTestSupervisor(t, testChildOversized)
	id, _, err := s.Render(figureRequestOf(KindSVG, `<svg/>`))
	if err != nil {
		t.Fatal(err)
	}
	o := awaitOutcome(t, s, id, helloTimeout)
	if o.Err == nil || o.Err.Code != FailureProtocol || !strings.Contains(o.Err.Message, "declares") {
		t.Fatalf("outcome = %+v, want a frame-length protocol failure", o.Err)
	}
}

func TestSupervisorPausesAfterThreeFailures(t *testing.T) {
	s := newTestSupervisor(t, testChildWrongNonce)
	for i := range FailureWindowLimit {
		id, _, err := s.Render(figureRequestOf(KindSVG, fmt.Sprintf(`<svg id="f%d"/>`, i)))
		if err != nil {
			t.Fatalf("render %d: %v", i, err)
		}
		awaitOutcome(t, s, id, helloTimeout)
	}
	if !s.Paused() {
		t.Fatal("supervisor kept rendering after three failures within the window")
	}
	if _, _, err := s.Render(figureRequestOf(KindSVG, `<svg id="later"/>`)); !errors.Is(err, ErrPaused) {
		t.Fatalf("render while paused = %v, want ErrPaused", err)
	}
	s.Resume()
	if _, _, err := s.Render(figureRequestOf(KindSVG, `<svg id="resumed"/>`)); err != nil {
		t.Fatalf("render after Resume = %v", err)
	}
}

func TestSupervisorQueueSaturationWaits(t *testing.T) {
	s := newTestSupervisor(t, testChildSilent)
	for i := range PendingJobLimit {
		if _, _, err := s.Render(figureRequestOf(KindSVG, fmt.Sprintf(`<svg id="q%d"/>`, i))); err != nil {
			t.Fatalf("render %d: %v", i, err)
		}
	}
	if _, _, err := s.Render(figureRequestOf(KindSVG, `<svg id="overflow"/>`)); !errors.Is(err, ErrWaiting) {
		t.Fatalf("render past the queue = %v, want ErrWaiting", err)
	}
}

func TestSupervisorDiscardsCancelledPendingJob(t *testing.T) {
	s := newTestSupervisor(t, testChildSilent)
	running, _, err := s.Render(figureRequestOf(KindSVG, `<svg id="running"/>`))
	if err != nil {
		t.Fatal(err)
	}
	for s.Launches() == 0 {
		time.Sleep(10 * time.Millisecond)
	}
	pending, _, err := s.Render(figureRequestOf(KindSVG, `<svg id="pending"/>`))
	if err != nil {
		t.Fatal(err)
	}
	s.Cancel(pending)
	s.Cancel(running)
	select {
	case o := <-s.Outcomes():
		t.Fatalf("cancelled jobs produced outcome %+v", o)
	case <-time.After(RenderDeadline + TerminationGrace + time.Second):
	}
	if got := s.Launches(); got != 1 {
		t.Fatalf("renderer launches = %d; the cancelled pending job must not launch a renderer", got)
	}
}

func TestReleasedRunningImageDoesNotAdmitLatePixels(t *testing.T) {
	s := &Supervisor{
		wake: make(chan struct{}, 1), pixels: NewPixelCache(NewPixelLedger(PixelCacheLimit)),
		images: map[uint64]imageResult{}, refs: map[PreviewKey]int{},
	}
	released := false
	req := ImageRequest{Format: ImagePNG, Data: []byte("image"), Scale: 1, Viewport: [2]float64{8, 8}, ReleaseSource: func() { released = true }}
	id, key, err := s.Image(req)
	if err != nil {
		t.Fatal(err)
	}
	running := s.take()
	s.ReleaseImage(id)
	if released {
		t.Fatal("cancellation released the image before the running request acknowledged it")
	}
	_, published := s.finish(running, Outcome{Job: id, Key: key, Image: solidPreview(8, 1)})
	if !released || published || s.Pixels().Used() != 0 {
		t.Fatalf("cancelled source released = %v, outcome published = %v, cache usage = %d bytes", released, published, s.Pixels().Used())
	}
}

func TestSupervisorRetriesFailedInputOnce(t *testing.T) {
	s := newTestSupervisor(t)
	broken := figureRequestOf(KindSVG, `<svg xmlns="http://www.w3.org/2000/svg"><script/></svg>`)
	id, _, err := s.Render(broken)
	if err != nil {
		t.Fatal(err)
	}
	if o := awaitOutcome(t, s, id, 3*helloTimeout); o.Err == nil || o.Err.Code != FailureForbidden {
		t.Fatalf("script figure outcome = %+v, want a forbidden failure", o.Err)
	}
	if _, _, err = s.Render(broken); !errors.Is(err, ErrFailedInput) {
		t.Fatalf("resubmitting failed input = %v, want ErrFailedInput", err)
	}
	broken.Retry = true
	id, _, err = s.Render(broken)
	if err != nil {
		t.Fatalf("explicit retry = %v", err)
	}
	awaitOutcome(t, s, id, 3*helloTimeout)
	if _, _, err := s.Render(broken); !errors.Is(err, ErrFailedInput) {
		t.Fatalf("second retry = %v, want ErrFailedInput", err)
	}
	if s.Launches() != 1 {
		t.Fatalf("invalid input restarted the renderer %d times", s.Launches()-1)
	}
}

func TestChildRefusesInvocationWithoutWorkerPipes(t *testing.T) {
	exe, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	devNull, err := os.Open(os.DevNull)
	if err != nil {
		t.Fatal(err)
	}
	defer devNull.Close()
	cmd := exec.Command(exe, ChildArgument)
	cmd.Stdin = devNull
	start := time.Now()
	out, err := cmd.CombinedOutput()
	var exit *exec.ExitError
	if !errors.As(err, &exit) || exit.ExitCode() != childExitInvalid {
		t.Fatalf("child without pipes = %v %q, want exit %d", err, out, childExitInvalid)
	}
	if time.Since(start) > 2*time.Second || !strings.Contains(string(out), "worker pipes") {
		t.Fatalf("child without pipes took %s and said %q", time.Since(start), out)
	}
}

func TestSupervisorTimesOutWhenRendererStopsReading(t *testing.T) {
	s := newTestSupervisor(t, testChildDeaf)
	data := make([]byte, 1<<20)
	released := make(chan struct{})
	id, _, err := s.Image(ImageRequest{Format: ImagePNG, Data: data, Scale: 1, Viewport: [2]float64{640, 480}, Visible: true, ReleaseSource: func() { close(released) }})
	if err != nil {
		t.Fatal(err)
	}
	started := time.Now()
	o := awaitOutcome(t, s, id, RenderDeadline+TerminationGrace+5*time.Second)
	select {
	case <-released:
	default:
		t.Fatal("timeout was published before the image request released its source")
	}
	if o.Err == nil || o.Err.Code != FailureTimeout {
		t.Fatalf("unread request outcome = %+v, want a timeout failure", o.Err)
	}
	if elapsed := time.Since(started); elapsed > RenderDeadline+TerminationGrace+time.Second {
		t.Fatalf("unread request timed out after %s; the deadline plus grace is %s", elapsed, RenderDeadline+TerminationGrace)
	}
	if got := s.Launches(); got != 1 {
		t.Fatalf("renderer launches = %d, want exactly one launch with no restart loop", got)
	}
	if s.Paused() {
		t.Fatal("supervisor paused after one unread request")
	}
}

func TestSupervisorHighlightNormalizesLanguage(t *testing.T) {
	s := newTestSupervisor(t)
	if _, _, err := s.Highlight(HighlightRequest{Document: "doc", Revision: 1, Language: "klingon", Source: []byte("x")}); err == nil {
		t.Fatal("Highlight of an unknown language was accepted instead of failing fast")
	}
	_, aliasKey, err := s.Highlight(HighlightRequest{Document: "doc", Revision: 1, Language: "ts", Source: []byte("const x = 1\n")})
	if err != nil {
		t.Fatal(err)
	}
	_, canonicalKey, err := s.Highlight(HighlightRequest{Document: "doc", Revision: 1, Language: "typescript", Source: []byte("const x = 1\n")})
	if err != nil {
		t.Fatal(err)
	}
	if aliasKey != canonicalKey {
		t.Fatalf("alias highlight key = %s, canonical key = %s, want one shared content key", aliasKey, canonicalKey)
	}
}
