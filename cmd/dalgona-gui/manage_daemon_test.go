//go:build gui

package main

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"fmt"
	"image"
	"io"
	"net"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"testing"
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

	"github.com/gosuda/dalgona/internal/dal/app"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/rpcserver"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/doctor"
	"github.com/gosuda/dalgona/internal/dalgona/gui/manage"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const daemonEndpoint client.EndpointID = "in-process"

const permission = "dalgona/workspace/requestPermission"

type daemonRig struct {
	t         *testing.T
	ctx       context.Context
	owner     *state.Owner
	set       *client.EndpointSet
	panels    *manage.Panels
	router    *input.Router
	shaper    *text.Shaper
	now       time.Time
	nodes     []input.SemanticNode
	thread    state.ThreadKey
	cwd       string
	closed    int
	announced []platform.Announcement
}

func newDaemonRig(t *testing.T) *daemonRig {
	t.Helper()
	home := t.TempDir()
	t.Setenv("DALGONA_HOME", home)
	cwd, err := filepath.EvalSymlinks(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	host, closeHost, err := app.NewHost(ctx, app.Options{Product: config.ProductDalgona})
	if err != nil {
		cancel()
		t.Fatalf("daemon host: %v", err)
	}
	options := nativeServeOptions(home)
	var serving sync.WaitGroup
	dial := func(context.Context) (io.ReadWriteCloser, error) {
		serverEnd, clientEnd := net.Pipe()
		serving.Go(func() { _ = rpcserver.ServeWith(ctx, serverEnd, host, options) })
		return clientEnd, nil
	}
	owner := state.New(func() {}, t.TempDir())
	set, err := client.NewEndpointSet(nil, []client.EndpointSpec{{ID: daemonEndpoint, Label: "in-process", Kind: client.EndpointLocal, Dial: dial}}, owner, client.ProtocolNative)
	if err != nil {
		cancel()
		t.Fatal(err)
	}
	running := make(chan struct{})
	go func() {
		defer close(running)
		_ = owner.Run(ctx, set)
	}()
	t.Cleanup(func() {
		cancel()
		<-running
		_ = set.Close()
		serving.Wait()
		_ = closeHost()
	})
	shaper, err := platform.NewShaper()
	if err != nil {
		t.Fatal(err)
	}
	hub := manage.NewHub(ctx, manage.Deps{Session: set, Invalidate: func() {}})
	r := &daemonRig{t: t, ctx: ctx, owner: owner, set: set, panels: manage.New(hub), router: new(input.Router), shaper: shaper, now: time.Unix(1_800_000_000, 0), cwd: cwd}
	r.start()
	return r
}

func nativeServeOptions(home string) rpcserver.ServeOptions {
	extensions := map[string]rpcserver.Extension{
		"dalgona/auth/providers":   unconfiguredService(),
		"dalgona/auth/start":       unconfiguredService(),
		"dalgona/auth/submit":      unconfiguredService(),
		"dalgona/auth/status":      unconfiguredService(),
		"dalgona/auth/cancel":      unconfiguredService(),
		"dalgona/auth/logout":      unconfiguredService(),
		"dalgona/mcp/config/read":  unconfiguredService(),
		"dalgona/mcp/config/write": unconfiguredService(),
		"dalgona/remote/status": rpcserver.WithDeadline(rpcserver.MetadataDeadline, func(context.Context, jsontext.Value) (jsontext.Value, error) {
			return jsontext.Value(`{"runners":[],"bindings":[],"ledgerRevision":"0"}`), nil
		}),
		"dalgona/remote/pull":       unconfiguredService(),
		"dalgona/remote/gc":         unconfiguredService(),
		"dalgona/diagnostics/run":   rpcserver.WithDeadline(rpcserver.PageDeadline, diagnoseHome(home)),
		"dalgona/reporting/read":    unconfiguredService(),
		"dalgona/reporting/enable":  unconfiguredService(),
		"dalgona/reporting/disable": unconfiguredService(),
	}
	return rpcserver.ServeOptions{
		Extensions: extensions,
		Terminals: rpcserver.NewTerminalHost(rpcserver.TerminalEngine{
			Version:       1,
			EngineVersion: "fixture",
			New:           func(columns, rows int) rpcserver.TerminalScreen { return newFixtureTerminalScreen(columns, rows) },
		}),
	}
}

type fixtureTerminalScreen struct {
	buf           []byte
	columns, rows int
}

func newFixtureTerminalScreen(columns, rows int) *fixtureTerminalScreen {
	return &fixtureTerminalScreen{columns: columns, rows: rows}
}

func (s *fixtureTerminalScreen) Write(p []byte) (int, error) {
	s.buf = append(s.buf, p...)
	return len(p), nil
}

func (s *fixtureTerminalScreen) Resize(columns, rows int) { s.columns, s.rows = columns, rows }

func (s *fixtureTerminalScreen) Checkpoint() ([]byte, error) {
	return append([]byte(nil), s.buf...), nil
}

func (s *fixtureTerminalScreen) TakeReplies() []byte { return nil }

func unconfiguredService() rpcserver.Extension {
	return func(context.Context, jsontext.Value) (jsontext.Value, error) {
		return nil, rpc.NativeKind(rpc.ErrorUnavailable)
	}
}

func diagnoseHome(home string) rpcserver.Extension {
	return func(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
		var online *bool
		err := rpc.ReadMembers(jsontext.NewDecoder(bytes.NewReader(params)), func(name string, d *jsontext.Decoder) error {
			if name != "online" {
				return d.SkipValue()
			}
			value, readErr := rpc.ReadBool(d)
			online = &value
			return readErr
		})
		if err != nil || online == nil {
			return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
		}
		rows := doctor.Diagnose(ctx, doctor.Env{
			Home: home, Dial: func(context.Context) (net.Conn, error) { return client.DialHome(home) },
			Own: config.Build(), Now: time.Now, Online: *online,
		})
		return encodeDiagnosticRows(rows)
	}
}

func encodeDiagnosticRows(rows []doctor.Row) (jsontext.Value, error) {
	if len(rows) > 1000 {
		return nil, rpc.NativeKind(rpc.ErrorLimitExceeded)
	}
	encoded, err := rpc.Marshal(diagnosticRows(rows))
	if err != nil {
		return nil, err
	}
	if len(encoded) > 1<<20 {
		return nil, rpc.NativeKind(rpc.ErrorLimitExceeded)
	}
	return encoded, nil
}

type diagnosticRows []doctor.Row

func (rows diagnosticRows) Encode(o *rpc.Writer) {
	o.Open()
	o.Key("rows")
	o.OpenArray()
	for _, row := range rows {
		o.Open()
		o.Str("status", row.Status)
		o.Str("check", row.Check)
		o.Str("detail", row.Detail)
		o.Close()
	}
	o.CloseArray()
	o.Int("exitCode", int64(doctor.ExitCode(rows)))
	o.Close()
}

func (r *daemonRig) view() *state.View { return r.owner.View() }

func (r *daemonRig) await(what string, ok func() bool) {
	r.t.Helper()
	deadline := time.Now().Add(20 * time.Second)
	for !ok() {
		if time.Now().After(deadline) {
			r.t.Fatalf("timed out waiting until %s; labels %s", what, r.labels())
		}
		time.Sleep(2 * time.Millisecond)
	}
}

func (r *daemonRig) link() client.Link {
	for _, e := range r.view().Endpoints() {
		if e.ID == daemonEndpoint {
			return e.Link
		}
	}
	return client.Link{}
}

func (r *daemonRig) start() {
	r.t.Helper()
	r.owner.Submit(state.Connect{Endpoint: daemonEndpoint})
	r.await("the daemon connects", func() bool {
		for _, e := range r.view().Endpoints() {
			if e.ID == daemonEndpoint && e.Live {
				return true
			}
		}
		return false
	})
	var started struct {
		Thread rpc.Thread `json:"thread"`
	}
	if err := r.set.Call(r.ctx, r.link(), "thread/start", rpc.ThreadStartParams{Cwd: r.cwd, Model: "openai/gpt-test"}, &started); err != nil {
		r.t.Fatalf("thread/start: %v", err)
	}
	r.thread = state.ThreadKey{Endpoint: daemonEndpoint, Thread: started.Thread.ID}
	r.owner.Submit(state.OpenView{Window: "w", Thread: r.thread})
	r.await("the thread view opens", func() bool {
		tv, ok := r.view().Thread(r.thread)
		return ok && tv.HistoryID != ""
	})
}

func (r *daemonRig) env() shell.Env {
	return shell.Env{
		Theme: theme.Night, Shaper: r.shaper, View: r.view(), Window: "w", Thread: r.thread, Modal: true,
		Submit: r.owner.Submit, Navigate: func(state.ThreadKey) {}, Open: func(shell.Route) {},
		Close:    func() { r.closed++ },
		Announce: func(a platform.Announcement) { r.announced = append(r.announced, a) },
	}
}

func (r *daemonRig) frame(p shell.Panel) {
	r.now = r.now.Add(16 * time.Millisecond)
	ops := new(op.Ops)
	gtx := layout.Context{Ops: ops, Now: r.now, Metric: unit.Metric{PxPerDp: 1, PxPerSp: 1}, Constraints: layout.Exact(image.Pt(1100, 2400)), Source: r.router.Source()}
	p.Layout(gtx, r.env())
	r.router.Frame(ops)
	r.nodes = r.router.AppendSemantics(r.nodes[:0])
}

func (r *daemonRig) until(p shell.Panel, what string, ok func() bool) {
	r.t.Helper()
	r.await(what, func() bool {
		r.frame(p)
		return ok()
	})
}

func (r *daemonRig) labels() string {
	var b strings.Builder
	for _, n := range r.nodes {
		if n.Desc.Label != "" {
			fmt.Fprintf(&b, "%q ", n.Desc.Label)
		}
	}
	return b.String()
}

func (r *daemonRig) shows(fragment string) bool {
	return slices.ContainsFunc(r.nodes, func(n input.SemanticNode) bool { return strings.Contains(n.Desc.Label, fragment) })
}

func (r *daemonRig) enabled(label string) (input.SemanticNode, bool) {
	at := slices.IndexFunc(r.nodes, func(n input.SemanticNode) bool {
		return n.Desc.Class == semantic.Button && n.Desc.Label == label && !n.Desc.Disabled
	})
	if at < 0 {
		return input.SemanticNode{}, false
	}
	return r.nodes[at], true
}

func (r *daemonRig) press(bounds image.Rectangle) {
	at := f32.Pt(float32(bounds.Min.X+bounds.Max.X)/2, float32(bounds.Min.Y+bounds.Max.Y)/2)
	r.router.Queue(
		pointer.Event{Kind: pointer.Press, Source: pointer.Mouse, Buttons: pointer.ButtonPrimary, Position: at},
		pointer.Event{Kind: pointer.Release, Source: pointer.Mouse, Buttons: pointer.ButtonPrimary, Position: at},
	)
}

func (r *daemonRig) click(p shell.Panel, label string) {
	r.t.Helper()
	var button input.SemanticNode
	r.until(p, "the "+label+" button is enabled", func() bool {
		var ok bool
		button, ok = r.enabled(label)
		return ok
	})
	r.press(button.Desc.Bounds)
	r.frame(p)
}

func (r *daemonRig) typeInto(p shell.Panel, label, value string) {
	r.t.Helper()
	var field input.SemanticNode
	r.until(p, "the "+label+" field shows", func() bool {
		for _, parent := range r.nodes {
			if parent.Desc.Label != label {
				continue
			}
			for _, n := range r.nodes {
				if n.ParentID == parent.ID && n.Desc.Class == semantic.Editor {
					field = n
					return true
				}
			}
		}
		return false
	})
	r.press(field.Desc.Bounds)
	r.frame(p)
	r.router.Queue(key.EditEvent{Range: key.Range{Start: 0, End: 1 << 20}, Text: value})
	r.frame(p)
	r.frame(p)
}

func (r *daemonRig) workspaceAsks() []state.RequestView {
	tv, ok := r.view().Thread(r.thread)
	if !ok {
		return nil
	}
	var out []state.RequestView
	for _, q := range tv.Requests {
		if q.Method == permission && q.Actionable {
			out = append(out, q)
		}
	}
	return out
}

func (r *daemonRig) allowWorkspace(p shell.Panel) {
	r.t.Helper()
	r.until(p, "the daemon asks for workspace access", func() bool { return len(r.workspaceAsks()) > 0 })
	for _, q := range r.workspaceAsks() {
		r.owner.Submit(state.Answer{Request: q.Key, Result: jsontext.Value(`{"decision":"allowOnce"}`)})
	}
	r.until(p, "the workspace answer lands", func() bool {
		for _, q := range r.workspaceAsks() {
			r.owner.Submit(state.Answer{Request: q.Key, Result: jsontext.Value(`{"decision":"allowOnce"}`)})
		}
		return len(r.workspaceAsks()) == 0
	})
}

func TestManagePanelsReadAndWriteWorkspaceFilesThroughARealDaemon(t *testing.T) {
	r := newDaemonRig(t)
	notes := filepath.Join(r.cwd, "notes.txt")
	if err := os.WriteFile(notes, []byte("alpha\nbeta\nbeta\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	p := r.panels.Files
	p.Opened(shell.Route{Panel: "files", Thread: r.thread})
	r.click(p, "Open workspace")
	r.allowWorkspace(p)
	r.click(p, "File notes.txt")
	r.typeInto(p, "Find in file", "beta")
	r.until(p, "the loaded text is searchable", func() bool { return r.shows("Find next (2 matches)") })
	if err := os.WriteFile(notes, []byte("changed on disk\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	r.typeInto(p, "File text", "mine\n")
	r.click(p, "Save")
	r.until(p, "the daemon reports the concurrent change", func() bool { return r.shows("Current version on the endpoint:\nchanged on disk") })
	if got, _ := os.ReadFile(notes); string(got) != "changed on disk\n" {
		t.Fatalf("a save against a changed revision wrote %q", got)
	}
	r.click(p, "Keep my version")
	r.until(p, "the resolved save lands", func() bool {
		got, _ := os.ReadFile(notes)
		return string(got) == "mine\n"
	})
	want := platform.Announcement{Kind: platform.AnnounceCompletion, Text: "Saved notes.txt on " + string(daemonEndpoint)}
	r.until(p, "the save is announced", func() bool { return slices.Contains(r.announced, want) })
}

func TestManagePanelsRenameAndArchiveThroughARealDaemon(t *testing.T) {
	r := newDaemonRig(t)
	rename := r.panels.Rename
	rename.Opened(shell.Route{Panel: "rename", Thread: r.thread})
	r.typeInto(rename, "New title", "Integration  title ✓")
	r.click(rename, "Rename")
	r.until(rename, "the rename finishes", func() bool { return r.closed == 1 })
	r.await("the view shows the authoritative title", func() bool {
		tv, ok := r.view().Thread(r.thread)
		return ok && tv.Thread.Name == "Integration  title ✓"
	})
	archive := r.panels.Archive
	archive.Opened(shell.Route{Panel: "archive", Thread: r.thread})
	r.click(archive, "Archive")
	r.until(archive, "the archive finishes", func() bool { return r.closed == 2 })
	var archived rpc.ThreadListResponse
	if err := r.set.Call(r.ctx, r.link(), "thread/list", rpc.ThreadListParams{Archived: true, Limit: 50}, &archived); err != nil {
		t.Fatalf("thread/list archived: %v", err)
	}
	if !slices.ContainsFunc(archived.Data, func(th rpc.Thread) bool { return th.ID == r.thread.Thread && th.Name == "Integration  title ✓" }) {
		t.Fatalf("archived threads %+v lack %s with its new title", archived.Data, r.thread.Thread)
	}
}
