//go:build !windows

package runner

import (
	"net"
	"os"
	"path/filepath"
	"slices"
	"sync"
	"testing"
)

type shimGate struct {
	dir      string
	phase    string
	listener net.Listener
	conn     net.Conn
	once     sync.Once
}

func newShimGate(t *testing.T, phase string) *shimGate {
	t.Helper()
	dir, err := os.MkdirTemp("/tmp", "gate")
	if err != nil {
		t.Fatal(err)
	}
	listener, err := net.Listen("unix", filepath.Join(dir, "s"))
	if err != nil {
		_ = os.RemoveAll(dir)
		t.Fatal(err)
	}
	gate := &shimGate{dir: dir, phase: phase, listener: listener}
	t.Cleanup(gate.release)
	return gate
}

func (g *shimGate) env(base []string) []string {
	if len(base) == 0 {
		base = os.Environ()
	}
	env := slices.Clone(base)
	return append(env, shimPauseEnvironment+"="+g.phase+":"+g.listener.Addr().String())
}

func (g *shimGate) await(t *testing.T) {
	t.Helper()
	conn, err := g.listener.Accept()
	if err != nil {
		t.Fatalf("shim never reached phase %s: %v", g.phase, err)
	}
	g.conn = conn
}

func (g *shimGate) release() {
	g.once.Do(func() {
		if g.conn != nil {
			_ = g.conn.Close()
		}
		_ = g.listener.Close()
		_ = os.RemoveAll(g.dir)
	})
}

func startGatedProcess(t *testing.T, local *Local, spec ProcessSpec, phase string) *shimGate {
	t.Helper()
	gate := newShimGate(t, phase)
	spec.Env = gate.env(spec.Env)
	startTestProcess(t, local, spec)
	t.Cleanup(gate.release)
	return gate
}
