//go:build gui

package main

import (
	"os"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dalgona/gui/figure"
)

func TestMain(m *testing.M) {
	if figure.IsChild(os.Args) {
		os.Exit(serveFigureChild())
	}
	os.Exit(m.Run())
}

func TestDesktopFigureSupervisorMatchesChildManifest(t *testing.T) {
	exe, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	sup, err := newFigureSupervisor(exe)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(sup.Close)
	req := figure.Request{
		Kind: figure.KindSVG, Source: []byte(`<svg width="2" height="2"><rect width="2" height="2"/></svg>`),
		Scale: 1, Viewport: [2]float64{2, 2}, Visible: true,
	}
	id, _, err := sup.Render(req)
	if err != nil {
		t.Fatal(err)
	}
	select {
	case result := <-sup.Outcomes():
		if result.Job != id || result.Err != nil || result.List == nil {
			t.Fatalf("desktop renderer outcome = %+v; want SVG display list", result)
		}
	case <-time.After(30 * time.Second):
		t.Fatal("the desktop renderer did not complete the handshake and SVG request")
	}
}
