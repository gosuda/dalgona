package tui

import (
	"context"
	"strings"
	"testing"
)

func TestRenderDiagramMermaid(t *testing.T) {
	t.Parallel()
	art, err := renderDiagram("mermaid", "graph LR\n  a --> b\n")
	if err != nil {
		t.Fatalf("mermaid render: %v", err)
	}
	if !strings.Contains(art, "a") || !strings.Contains(art, "b") {
		t.Fatalf("mermaid art missing nodes: %q", art)
	}
}

func TestRenderDiagramD2(t *testing.T) {
	t.Parallel()
	art, err := renderDiagram("d2", "x -> y\n")
	if err != nil {
		t.Fatalf("d2 render: %v", err)
	}
	if !strings.Contains(art, "x") || !strings.Contains(art, "y") {
		t.Fatalf("d2 art missing nodes: %q", art)
	}
}

func TestRenderDiagramNomnoml(t *testing.T) {
	t.Parallel()
	budget := diagramBudget
	if raceEnabled {
		budget *= 4
	}
	ctx, cancel := context.WithTimeout(context.Background(), budget)
	defer cancel()
	art, err := renderNomnoml(ctx, "[A]->[B]\n")
	if err != nil {
		t.Fatalf("nomnoml render: %v", err)
	}
	if !strings.Contains(art, "A") || !strings.Contains(art, "B") {
		t.Fatalf("nomnoml art missing nodes: %q", art)
	}
}

func TestRenderDiagramSvgShowsSource(t *testing.T) {
	t.Parallel()
	if _, err := renderDiagram("svg", "<svg/>"); err == nil {
		t.Fatal("svg must fail to the source card")
	}
}

func TestRenderDiagramBlockHeader(t *testing.T) {
	t.Parallel()
	m := &model{}
	lines := m.renderDiagramBlock(block{text: "x -> y\n", diagram: "d2"})
	if len(lines) < 2 || !strings.HasPrefix(lines[0], "diagram d2 · ") || !strings.HasSuffix(lines[0], " rows") {
		t.Fatalf("diagram header: %#v", lines[:1])
	}
}

func TestRenderDiagramBlockFailure(t *testing.T) {
	t.Parallel()
	m := &model{}
	lines := m.renderDiagramBlock(block{text: "<svg/>", diagram: "svg"})
	if !strings.HasPrefix(lines[0], "diagram svg: render failed") || !strings.HasSuffix(lines[0], "source shown") {
		t.Fatalf("failure header: %#v", lines[:1])
	}
	if lines[1] != "<svg/>" {
		t.Fatalf("failure must show source: %#v", lines)
	}
}
