package tui

import (
	"context"
	"fmt"
	"io"
	"os"
	"strings"
	"sync"
	"testing"
	"time"
)

var diagramMu sync.Mutex

func TestMain(m *testing.M) {
	diagramBudget = 30 * time.Second
	os.Exit(m.Run())
}

func TestRenderDiagramMermaid(t *testing.T) {
	diagramMu.Lock()
	defer diagramMu.Unlock()
	art, err := renderDiagram("mermaid", "graph LR\n  a --> b\n")
	if err != nil {
		t.Fatalf("mermaid render: %v", err)
	}
	if !strings.Contains(art, "a") || !strings.Contains(art, "b") {
		t.Fatalf("mermaid art missing nodes: %q", art)
	}
}

func TestRenderDiagramD2(t *testing.T) {
	diagramMu.Lock()
	defer diagramMu.Unlock()
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
	diagramMu.Lock()
	defer diagramMu.Unlock()
	if _, err := renderDiagram("svg", "<svg/>"); err == nil {
		t.Fatal("svg must fail to the source card")
	}
}

func TestRenderDiagramBlockHeader(t *testing.T) {
	diagramMu.Lock()
	defer diagramMu.Unlock()
	m := &model{}
	lines := m.renderDiagramBlock(block{text: "x -> y\n", diagram: "d2"})
	if len(lines) < 2 || !strings.HasPrefix(lines[0], "diagram d2 · ") || !strings.HasSuffix(lines[0], " rows") {
		t.Fatalf("diagram header: %#v", lines[:1])
	}
}

func TestRenderDiagramBlockFailure(t *testing.T) {
	diagramMu.Lock()
	defer diagramMu.Unlock()
	m := &model{}
	lines := m.renderDiagramBlock(block{text: "<svg/>", diagram: "svg"})
	if !strings.HasPrefix(lines[0], "diagram svg: render failed") || !strings.HasSuffix(lines[0], "source shown") {
		t.Fatalf("failure header: %#v", lines[:1])
	}
	if lines[1] != "<svg/>" {
		t.Fatalf("failure must show source: %#v", lines)
	}
}

func captureTerminal(t *testing.T, run func()) string {
	t.Helper()
	reader, writer, err := os.Pipe()
	if err != nil {
		t.Fatalf("pipe: %v", err)
	}
	stdout, stderr := os.Stdout, os.Stderr
	t.Cleanup(func() {
		os.Stdout, os.Stderr = stdout, stderr
	})
	captured := make(chan string)
	go func() {
		data, _ := io.ReadAll(reader)
		captured <- string(data)
	}()
	os.Stdout, os.Stderr = writer, writer
	run()
	os.Stdout, os.Stderr = stdout, stderr
	if err := writer.Close(); err != nil {
		t.Fatalf("close pipe: %v", err)
	}
	out := <-captured
	if err := reader.Close(); err != nil {
		t.Fatalf("close reader: %v", err)
	}
	return out
}

func TestD2RenderWritesNothingToStdout(t *testing.T) {
	diagramMu.Lock()
	defer diagramMu.Unlock()
	var chain strings.Builder
	for i := range 40 {
		fmt.Fprintf(&chain, "n%d -> n%d: hop\n", i, i+1)
	}
	cases := []struct {
		name   string
		source string
		fails  bool
	}{
		{"small", "x -> y: hello\ny -> z: world\n", false},
		{"big", chain.String(), false},
		{"invalid", "x -> {\n", true},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			var err error
			out := captureTerminal(t, func() {
				_, err = renderDiagram("d2", c.source)
			})
			if (err != nil) != c.fails {
				t.Fatalf("render error = %v, want failure %v", err, c.fails)
			}
			if out != "" {
				t.Fatalf("d2 render wrote %d bytes to the terminal:\n%s", len(out), out)
			}
		})
	}
}
