//go:build gui && guiintegration

package platform

import (
	"context"
	"errors"
	"fmt"
	"os"
	"sync"
	"testing"

	"gioui.org/app"
	"gioui.org/io/clipboard"
	"gioui.org/io/system"
	"gioui.org/io/transfer"
	"gioui.org/op"
	"gioui.org/unit"
)

var smokeEvidence struct {
	sync.Mutex
	checks []Check
}

func TestMain(m *testing.M) {
	go func() {
		code := m.Run()
		if err := writeSmokeEvidence(); err != nil {
			fmt.Fprintln(os.Stderr, err)
			code = 1
		}
		os.Exit(code)
	}()
	app.Main()
}

func writeSmokeEvidence() error {
	path := os.Getenv("DALGONA_EVIDENCE_RECORD")
	if path == "" {
		return nil
	}
	host, err := os.Open(os.Getenv("DALGONA_EVIDENCE_HOST"))
	if err != nil {
		return fmt.Errorf("open the evidence host description named by DALGONA_EVIDENCE_HOST: %w", err)
	}
	rec, err := DecodeEvidence(host)
	if closeErr := host.Close(); err == nil {
		err = closeErr
	}
	if err != nil {
		return err
	}
	smokeEvidence.Lock()
	rec.Checks = append(rec.Checks, smokeEvidence.checks...)
	smokeEvidence.Unlock()
	out, err := os.Create(path)
	if err != nil {
		return fmt.Errorf("create the evidence record: %w", err)
	}
	if err := EncodeEvidence(out, rec); err != nil {
		return errors.Join(err, out.Close())
	}
	return out.Close()
}

func recordGate(t *testing.T, gate Gate, expected string) {
	t.Cleanup(func() {
		outcome, actual := OutcomePassed, "test passed"
		switch {
		case t.Skipped():
			outcome, actual = OutcomeSkipped, "test skipped"
		case t.Failed():
			outcome, actual = OutcomeFailed, "test failed; read the go test log"
		}
		smokeEvidence.Lock()
		defer smokeEvidence.Unlock()
		smokeEvidence.checks = append(smokeEvidence.checks, Check{
			Gate:      gate,
			Kind:      CheckAutomated,
			Procedure: fmt.Sprintf("go test -tags gui,guiintegration -count=1 -run '^%s$' ./internal/dalgona/gui/platform/", t.Name()),
			Expected:  expected,
			Actual:    actual,
			Outcome:   outcome,
			Artifact:  os.Getenv("DALGONA_EVIDENCE_LOG"),
		})
	})
}

func smokeWindow() *app.Window {
	w := new(app.Window)
	w.Option(app.Title("dalgona platform smoke"), app.Size(unit.Dp(320), unit.Dp(320)))
	return w
}

type generationProbe struct {
	lifetime   *Lifetime
	generation Generation
	pending    context.Context
	released   int
	early      bool
}

func (p *generationProbe) view(ctx context.Context, valid bool) {
	if !valid {
		p.lifetime.End()
		return
	}
	g, pending := p.lifetime.Begin(ctx)
	p.generation, p.pending = g, pending
	p.lifetime.Attach(g, func() {
		p.early = p.early || pending.Err() == nil
		p.released++
	})
}

func (p *generationProbe) check(t *testing.T, round int) {
	t.Helper()
	switch {
	case p.generation == 0:
		t.Fatalf("round %d: the backend sent no valid view event", round)
	case p.released != 1 || p.early:
		t.Fatalf("round %d: adapter released %d times, before cancellation %v", round, p.released, p.early)
	case p.lifetime.Current(p.generation) || p.pending.Err() == nil:
		t.Fatalf("round %d: the closed window generation stayed live", round)
	}
}

func TestHandleLifecycleReleasesEveryGeneration(t *testing.T) {
	recordGate(t, GateHandleLifecycle, "each closed window cancels its pending work, then releases each adapter of its generation once")
	lifetime := new(Lifetime)
	var previous Generation
	for round := range 3 {
		probe := &generationProbe{lifetime: lifetime}
		w := smokeWindow()
		for destroyed := false; !destroyed; {
			switch e := w.Event().(type) {
			case app.ViewEvent:
				probe.view(t.Context(), e.Valid())
			case app.FrameEvent:
				e.Frame(new(op.Ops))
				w.Perform(system.ActionClose)
			case app.DestroyEvent:
				lifetime.End()
				if e.Err != nil {
					t.Fatalf("round %d: window failed: %v", round, e.Err)
				}
				destroyed = true
			}
		}
		probe.check(t, round)
		if probe.generation <= previous {
			t.Fatalf("round %d reused generation %d", round, probe.generation)
		}
		previous = probe.generation
	}
}

type clipboardProbe struct {
	step int
	text string
	err  error
}

func (p *clipboardProbe) frame(w *app.Window, e app.FrameEvent) error {
	gtx := app.NewContext(new(op.Ops), e)
	switch p.step {
	case 0:
		if err := WriteText(gtx.Source, exactSample); err != nil {
			return err
		}
		p.step = 1
		w.Invalidate()
	case 1:
		gtx.Execute(clipboard.ReadCmd{Tag: p})
		p.step = 2
	default:
		if ev, ok := gtx.Event(PasteFilter(p)); ok {
			p.text, p.err = ReadPaste(ev.(transfer.DataEvent))
			w.Perform(system.ActionClose)
		}
	}
	e.Frame(gtx.Ops)
	return nil
}

func TestClipboardRoundTripIsExact(t *testing.T) {
	recordGate(t, GateClipboard, "the native clipboard returns the exact written text, including CRLF, combining marks, and joined emoji")
	w := smokeWindow()
	probe := new(clipboardProbe)
	for {
		switch e := w.Event().(type) {
		case app.FrameEvent:
			if err := probe.frame(w, e); err != nil {
				t.Fatal(err)
			}
		case app.DestroyEvent:
			if e.Err != nil {
				t.Fatal(e.Err)
			}
			if probe.err != nil || probe.text != exactSample {
				t.Fatalf("clipboard returned %q, error %v; want %q", probe.text, probe.err, exactSample)
			}
			return
		}
	}
}
