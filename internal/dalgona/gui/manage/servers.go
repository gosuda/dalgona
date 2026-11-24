//go:build gui

package manage

import (
	"encoding/json/jsontext"
	"encoding/json/v2"
	"errors"
	"fmt"
	"net/url"
	"slices"
	"strconv"
	"strings"

	"gioui.org/layout"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const (
	statusPageLimit  = 100
	mcpEditLimit     = 32
	mcpFieldBytes    = 16384
	sourceUser       = "user"
	authNotLoggedIn  = "notLoggedIn"
	runtimeStarting  = "starting"
	runtimeConnected = "connected"
	runtimeFailed    = "failed"
)

var (
	mcpLinePolicy  = inputtext.Policy{Lines: inputtext.SingleLine, Limit: inputtext.Limit{Unit: inputtext.Bytes, Max: mcpFieldBytes}}
	mcpLinesPolicy = inputtext.Policy{Lines: inputtext.MultiLine, Limit: inputtext.Limit{Unit: inputtext.Bytes, Max: mcpFieldBytes}}
	errNoSource    = errors.New("the daemon returned MCP runtime status without its configuration source; upgrade the daemon (mcpConfig:1 requires provenance)")
	errOtherConfig = errors.New("the runtime status answers another configuration version")
)

type envEntry struct {
	key      string
	redacted bool
	plain    string
	replace  toggle
	drop     toggle
	value    *field
}

type serverEditor struct {
	open     bool
	existing string
	name     *field
	http     toggle
	command  *field
	args     *field
	env      []*envEntry
	newKey   *field
	newValue *field
	addEnv   widget.Clickable
	url      *field
	bearer   *field
	oauth    toggle
	scopes   *field
	clientID *field
	port     *field
	save     widget.Clickable
	discard  widget.Clickable
	problems []string
}

func newServerEditor() serverEditor {
	return serverEditor{
		name: newField(mcpLinePolicy, false), command: newField(mcpLinePolicy, false), args: newField(mcpLinesPolicy, false),
		newKey: newField(mcpLinePolicy, false), newValue: newField(mcpLinePolicy, true), url: newField(mcpLinePolicy, false),
		bearer: newField(mcpLinePolicy, false), scopes: newField(mcpLinePolicy, false), clientID: newField(mcpLinePolicy, false),
		port: newField(mcpLinePolicy, false),
	}
}

func (e *serverEditor) fill(s mcpServer) {
	e.open, e.existing, e.problems = true, s.Name, nil
	e.name.set(s.Name)
	c := s.Config
	e.http.on, e.oauth.on = c.Kind == "http", c.OAuth
	e.command.set(c.Command)
	e.args.set(strings.Join(c.Args, "\n"))
	e.url.set(c.URL)
	e.bearer.set(c.BearerTokenEnv)
	e.scopes.set(strings.Join(c.OAuthScopes, " "))
	e.clientID.set(c.OAuthClientID)
	e.port.set("")
	if c.OAuthCallbackPort > 0 {
		e.port.set(strconv.Itoa(c.OAuthCallbackPort))
	}
	e.env = nil
	keys := make([]string, 0, len(c.Env))
	for k := range c.Env {
		keys = append(keys, k)
	}
	slices.Sort(keys)
	for _, k := range keys {
		v := c.Env[k]
		entry := &envEntry{key: k, redacted: v.Redacted, value: newField(mcpLinePolicy, true)}
		if v.Value != nil {
			entry.plain = *v.Value
			entry.value.set(*v.Value)
		}
		e.env = append(e.env, entry)
	}
	e.newKey.set("")
	e.newValue.set("")
}

func (e *serverEditor) fields() []*field {
	out := []*field{e.name, e.command, e.args, e.newKey, e.newValue, e.url, e.bearer, e.scopes, e.clientID, e.port}
	for _, entry := range e.env {
		out = append(out, entry.value)
	}
	return out
}

func (e *serverEditor) update(gtx layout.Context, off bool) {
	for _, f := range e.fields() {
		f.update(gtx)
	}
	e.http.update(gtx, off)
	e.oauth.update(gtx, off)
	for _, entry := range e.env {
		entry.replace.update(gtx, off || entry.drop.on)
		entry.drop.update(gtx, off)
	}
	if clicked(gtx, &e.addEnv, off) {
		e.addVariable()
	}
}

func (e *serverEditor) addVariable() {
	key := e.newKey.text()
	if key == "" || strings.ContainsAny(key, "= ") || slices.ContainsFunc(e.env, func(x *envEntry) bool { return x.key == key }) {
		e.problems = []string{"Enter a new variable name without spaces or '='."}
		return
	}
	entry := &envEntry{key: key, value: newField(mcpLinePolicy, true)}
	entry.value.set(e.newValue.text())
	entry.replace.on = true
	e.env = append(e.env, entry)
	e.newKey.set("")
	e.newValue.set("")
	e.problems = nil
}

func (e *serverEditor) config() (mcpConfig, []string) {
	if e.http.on {
		return e.httpConfig()
	}
	var problems []string
	if inputtext.Blank(e.command.text()) {
		problems = append(problems, "A stdio server needs a command.")
	}
	env := map[string]mcpEnvValue{}
	for _, entry := range e.env {
		if entry.drop.on {
			continue
		}
		env[entry.key] = entry.wire()
	}
	args := []string{}
	for line := range strings.Lines(e.args.text()) {
		if arg := strings.TrimRight(line, "\r\n"); arg != "" {
			args = append(args, arg)
		}
	}
	return mcpConfig{Kind: "stdio", Command: e.command.text(), Args: args, Env: env}, problems
}

func (entry *envEntry) wire() mcpEnvValue {
	if entry.redacted && !entry.replace.on {
		return mcpEnvValue{Preserve: true}
	}
	v := entry.value.text()
	return mcpEnvValue{Value: &v}
}

func (e *serverEditor) httpConfig() (mcpConfig, []string) {
	var problems []string
	u, err := url.Parse(e.url.text())
	if err != nil || (u.Scheme != "http" && u.Scheme != "https") || u.Host == "" {
		problems = append(problems, "An HTTP server needs an http or https URL with a host.")
	}
	c := mcpConfig{Kind: "http", URL: e.url.text(), BearerTokenEnv: e.bearer.text(), OAuth: e.oauth.on, OAuthScopes: strings.Fields(e.scopes.text()), OAuthClientID: e.clientID.text()}
	c.OAuthCallbackPort, problems = e.httpPort(c.OAuthCallbackPort, problems)
	problems = e.httpAuth(problems)
	return c, problems
}

func (e *serverEditor) httpPort(current int, problems []string) (int, []string) {
	if p := e.port.text(); p != "" {
		n, err := strconv.Atoi(p)
		if err != nil || n < 1 || n > 65535 {
			return current, append(problems, "The OAuth callback port must be a number from 1 to 65535.")
		}
		return n, problems
	}
	return current, problems
}

func (e *serverEditor) httpAuth(problems []string) []string {
	if e.bearer.text() != "" && e.oauth.on {
		return append(problems, "Choose either a bearer token variable or OAuth, not both.")
	}
	return problems
}

type Servers struct {
	base
	target        state.ThreadKey
	inv           *mcpInventory
	statuses      []mcpRuntime
	loading       bool
	statusLoading bool
	invErr        error
	statusErr     error
	staleReloads  int
	epoch         uint64
	reload        widget.Clickable
	add           widget.Clickable
	edits         choice
	removes       choice
	signIns       choice
	editor        serverEditor
	write         mutation
	pending       []mcpEdit
	restart       []string
	saved         []string
	conflict      bool
	auth          *authFlow
	authFor       string
}

func newServers(h *Hub) *Servers {
	s := &Servers{editor: newServerEditor(), auth: newAuthFlow()}
	s.init(h)
	return s
}

func (s *Servers) Opened(route shell.Route) {
	s.reopen()
	s.target = route.Thread
	s.reset()
	s.editor.open, s.conflict, s.restart, s.saved, s.authFor = false, false, nil, nil, ""
	s.write.state, s.write.err = outcomeIdle, nil
}

func (s *Servers) reset() {
	s.epoch++
	s.inv, s.statuses, s.loading, s.statusLoading, s.invErr, s.statusErr = nil, nil, false, false, nil, nil
}

func (s *Servers) Layout(gtx layout.Context, host shell.Env) layout.Dimensions {
	env := s.scene(host)
	s.work.settle()
	b := bind(env.View, s.target.Endpoint)
	gate := b.gate(featureMCPConfig, "MCP configuration")
	if gate == "" && s.inv == nil && !s.loading && s.invErr == nil {
		s.load(b)
	}
	if clicked(gtx, &s.reload, s.loading || gate != "") {
		s.staleReloads = 0
		s.reset()
		s.load(b)
	}
	s.updateRows(gtx, env, b)
	s.updateEditor(gtx, b)
	s.hub.checkOutcome(gtx, &s.work, env.View, &s.write, func(raw jsontext.Value) {
		var res mcpWritten
		if json.Unmarshal(raw, &res) == nil {
			s.written(b, res, nil)
		}
	})
	s.updateAuth(gtx, env, b)
	s.flush(env)
	return s.show(gtx, env, page{title: "MCP servers", body: s.body(env, b, gate), footer: s.footer(gate)})
}

func (s *Servers) updateAuth(gtx layout.Context, env scene, b binding) {
	if s.authFor == "" {
		return
	}
	ep, _ := endpointView(env.View, s.target.Endpoint)
	s.auth.update(gtx, authWork{hub: s.hub, work: &s.work, view: env.View, bind: b}, ep.Kind)
	if s.auth.reloadClicked(gtx) {
		s.auth.load(s.hub, &s.work, b, s.target.Thread)
	}
}

func (s *Servers) footer(gate string) []action {
	return []action{
		{click: &s.add, label: "Add user server", variant: secondary, off: gate != "" || s.inv == nil || s.editor.open},
		{click: &s.reload, label: "Reload", variant: quiet, off: s.loading || gate != ""},
	}
}

func (s *Servers) load(b binding) {
	s.loading, s.invErr, s.statusErr = true, nil, nil
	epoch, thread := s.epoch, s.target.Thread
	s.work.spawn(func() func() {
		var inv mcpInventory
		err := s.hub.call(b, pageDeadline, methodMCPConfigRead, mcpReadParams{ThreadID: thread}, &inv)
		return func() {
			if s.epoch != epoch {
				return
			}
			s.loading, s.invErr = false, err
			if err == nil {
				s.inv = &inv
				s.loadStatus(b, thread, inv.Version)
			}
		}
	})
}

func (s *Servers) loadStatus(b binding, thread, version string) {
	s.statusLoading, s.statusErr, s.statuses = true, nil, nil
	epoch := s.epoch
	s.work.spawn(func() func() {
		rows, err := s.statusPages(b, thread, version)
		return func() {
			if s.epoch != epoch {
				return
			}
			s.statusLoading = false
			if kindOf(err) == rpc.ErrorStaleRevision || errors.Is(err, errOtherConfig) {
				s.restale(b)
				return
			}
			s.statusErr, s.statuses = err, rows
		}
	})
}

func (s *Servers) restale(b binding) {
	s.staleReloads++
	if s.staleReloads > 2 {
		s.statusErr = errors.New("the MCP configuration keeps changing while its status loads; reload when it settles")
		return
	}
	s.reset()
	s.load(b)
}

func (s *Servers) statusPages(b binding, thread, version string) ([]mcpRuntime, error) {
	var rows []mcpRuntime
	cursor := ""
	for range statusPageLimit {
		var page mcpStatusPage
		if err := s.hub.call(b, pageDeadline, methodMCPStatus, mcpStatusParams{ThreadID: thread, Cursor: cursor, ConfigVersion: version}, &page); err != nil {
			return nil, err
		}
		if page.ConfigVersion != version {
			return nil, errOtherConfig
		}
		if slices.ContainsFunc(page.Data, func(r mcpRuntime) bool { return r.Source == "" }) {
			return nil, errNoSource
		}
		rows = append(rows, page.Data...)
		if cursor = page.NextCursor; cursor == "" {
			return rows, nil
		}
	}
	return nil, fmt.Errorf("MCP status exceeded %d pages; reload", statusPageLimit)
}

func (s *Servers) runtime(server mcpServer) *mcpRuntime {
	if !server.Effective {
		return nil
	}
	at := slices.IndexFunc(s.statuses, func(r mcpRuntime) bool { return r.Name == server.Name && r.Source == server.Source })
	if at < 0 {
		return nil
	}
	return &s.statuses[at]
}

func (s *Servers) servers() []mcpServer {
	if s.inv == nil {
		return nil
	}
	return s.inv.Servers
}

func (s *Servers) updateRows(gtx layout.Context, env scene, b binding) {
	servers := s.servers()
	off := s.write.running() || s.editor.open
	s.updateRowEdits(gtx, b, servers, off)
	s.updateRowSignIn(gtx, b, servers)
	if clicked(gtx, &s.add, s.inv == nil || s.editor.open) {
		s.editor.fill(mcpServer{Config: mcpConfig{Kind: "stdio"}})
		s.editor.existing = ""
	}
}

func (s *Servers) updateRowEdits(gtx layout.Context, b binding, servers []mcpServer, off bool) {
	if at := s.edits.picked(gtx, len(servers)); at >= 0 && !off && servers[at].Editable {
		s.editor.fill(servers[at])
	}
	if at := s.removes.picked(gtx, len(servers)); at >= 0 && !off && servers[at].Editable {
		s.submit(b, []mcpEdit{{Name: servers[at].Name, Action: "remove"}})
	}
}

func (s *Servers) updateRowSignIn(gtx layout.Context, b binding, servers []mcpServer) {
	if at := s.signIns.picked(gtx, len(servers)); at >= 0 {
		s.authFor = servers[at].Name
		s.auth.bind(authTarget{Kind: "mcp", Name: servers[at].Name, ThreadID: s.target.Thread}, servers[at].Name)
		s.auth.load(s.hub, &s.work, b, s.target.Thread)
	}
}

func (s *Servers) updateEditor(gtx layout.Context, b binding) {
	if !s.editor.open {
		return
	}
	s.editor.update(gtx, s.write.running())
	if clicked(gtx, &s.editor.discard, s.write.running()) {
		s.editor.open, s.conflict = false, false
	}
	if clicked(gtx, &s.editor.save, s.write.running()) {
		s.saveEditor(b)
	}
}

func (s *Servers) saveEditor(b binding) {
	e := &s.editor
	name := e.name.text()
	c, problems := e.config()
	if inputtext.Blank(name) || strings.ContainsAny(name, " \t") {
		problems = append(problems, "A server needs a name without spaces.")
	}
	if e.existing == "" && slices.ContainsFunc(s.servers(), func(x mcpServer) bool { return x.Name == name && x.Source == sourceUser }) {
		problems = append(problems, "A user server named "+name+" already exists; edit it instead.")
	}
	if e.problems = problems; len(problems) > 0 {
		return
	}
	s.submit(b, []mcpEdit{{Name: name, Action: "put", Config: &c}})
}

func (s *Servers) submit(b binding, edits []mcpEdit) {
	if len(edits) > mcpEditLimit || s.inv == nil {
		return
	}
	id := s.write.begin(methodMCPConfigWrite, b, true)
	s.pending, s.conflict, s.restart, s.saved = edits, false, nil, nil
	params := mcpWriteParams{ExpectedVersion: s.inv.Version, Edits: edits, OperationID: id}
	s.work.spawn(func() func() {
		var res mcpWritten
		err := s.hub.call(b, pageDeadline, methodMCPConfigWrite, params, &res)
		return func() {
			if s.write.id != id {
				return
			}
			s.write.settle(err)
			s.written(b, res, err)
		}
	})
}

func (s *Servers) written(b binding, res mcpWritten, err error) {
	if kindOf(err) == rpc.ErrorStaleRevision {
		s.conflict = true
		s.write.state, s.write.err = outcomeIdle, nil
		return
	}
	if err != nil {
		return
	}
	s.editor.open = false
	s.restart = res.RestartRequiredServers
	for _, e := range s.pending {
		s.saved = append(s.saved, e.Name)
	}
	s.announce(platform.AnnounceCompletion, "MCP configuration saved")
	s.staleReloads = 0
	s.reset()
	s.load(b)
}

func (s *Servers) body(env scene, b binding, gate string) []layout.Widget {
	body := []layout.Widget{targetLine(env, s.target)}
	if gate != "" {
		return append(body, sayer(env, toneWarning, gate))
	}
	switch {
	case s.invErr != nil:
		return append(body, sayer(env, toneFailed, "MCP configuration did not load: "+explain(s.invErr)))
	case s.inv == nil:
		return append(body, sayer(env, toneDim, "Loading MCP configuration…"))
	}
	body = append(body, s.trust(env), s.write.line(env))
	body = append(body, s.saveNotes(env)...)
	if s.editor.open {
		body = append(body, s.editorWidgets(env)...)
	}
	if s.authFor != "" {
		ep, _ := endpointView(env.View, s.target.Endpoint)
		body = append(body, sayer(env, toneHeading, "Sign in to "+s.authFor))
		body = append(body, s.auth.widgets(env, ep.Kind)...)
	}
	body = append(body, s.statusNote(env))
	servers := s.servers()
	if len(servers) == 0 {
		body = append(body, sayer(env, toneBody, "No MCP servers are configured for this thread."))
	}
	s.edits.grow(len(servers))
	s.removes.grow(len(servers))
	s.signIns.grow(len(servers))
	for i, server := range servers {
		body = append(body, s.serverRow(env, i, server))
	}
	return body
}

func (s *Servers) trust(env scene) layout.Widget {
	if s.inv.TrustedWorkspace {
		return sayer(env, toneDim, "This workspace is trusted; its project servers may start.")
	}
	return sayer(env, toneWarning, "This workspace is not trusted. Its project servers stay stopped until you approve the trust request that the thread's next turn shows with every command, argument, and variable name. Inspecting them here runs nothing.")
}

func (s *Servers) saveNotes(env scene) []layout.Widget {
	var out []layout.Widget
	if s.conflict {
		out = append(out, sayer(env, toneWarning, "The configuration changed on the daemon since it loaded. Your edits are kept below; reload to see the change, then save again."))
	}
	if len(s.saved) > 0 {
		out = append(out, sayer(env, toneAdded, "Saved: "+strings.Join(s.saved, ", ")+"."))
	}
	if len(s.restart) > 0 {
		out = append(out, sayer(env, toneWarning, "Restart these workspace servers to use the new configuration: "+strings.Join(s.restart, ", ")+"."))
	}
	if names := s.needAuth(); len(names) > 0 {
		out = append(out, sayer(env, toneWarning, "These saved servers need sign-in: "+strings.Join(names, ", ")+"."))
	}
	return out
}

func (s *Servers) needAuth() []string {
	var out []string
	for _, name := range s.saved {
		for _, r := range s.statuses {
			if r.Name == name && r.Source == sourceUser && r.AuthStatus == authNotLoggedIn {
				out = append(out, name)
			}
		}
	}
	return out
}

func (s *Servers) statusNote(env scene) layout.Widget {
	switch {
	case s.statusLoading:
		return sayer(env, toneDim, "Loading runtime status…")
	case s.statusErr != nil:
		return sayer(env, toneFailed, "Runtime status did not load: "+explain(s.statusErr))
	}
	return nil
}

func sourceText(server mcpServer) string {
	switch server.Source {
	case sourceUser:
		return "user configuration, editable"
	case "project":
		return "workspace configuration, read-only here"
	case "discovered":
		return "discovered configuration, read-only here"
	}
	return server.Source + " configuration, read-only here"
}

func (s *Servers) runtimeText(server mcpServer) (tone, string) {
	if !server.Effective {
		return toneDim, "Shadowed by an earlier entry with the same name; it does not run."
	}
	if s.statusErr != nil || s.statusLoading {
		return toneDim, "Runtime state unknown."
	}
	r := s.runtime(server)
	switch {
	case r == nil:
		return toneDim, "Not started."
	case r.AuthStatus == authNotLoggedIn:
		return toneWarning, "Authentication required."
	case r.RuntimeStatus == runtimeFailed:
		return toneFailed, "Failed: " + cmpOr(r.ToolsError, "no reason given")
	case r.RuntimeStatus == runtimeStarting:
		return toneDim, "Starting."
	case r.RuntimeStatus == runtimeConnected:
		return toneAdded, fmt.Sprintf("Connected with %d tools.", len(r.Tools))
	}
	return toneDim, r.RuntimeStatus
}

func transportText(c mcpConfig) string {
	if c.Kind == "http" {
		auth := "no authentication"
		switch {
		case c.OAuth:
			auth = "OAuth"
		case c.BearerTokenEnv != "":
			auth = "bearer token from $" + c.BearerTokenEnv
		}
		return fmt.Sprintf("HTTP %s · %s", c.URL, auth)
	}
	return fmt.Sprintf("stdio %s %s", c.Command, strings.Join(c.Args, " "))
}

func (s *Servers) serverRow(env scene, i int, server mcpServer) layout.Widget {
	t, text := s.runtimeText(server)
	return func(gtx layout.Context) layout.Dimensions {
		items := []layout.Widget{
			sayer(env, toneHeading, server.Name),
			sayer(env, toneDim, sourceText(server)),
			sayer(env, toneCode, transportText(server.Config)),
			sayer(env, t, text),
		}
		if r := s.runtime(server); r != nil && len(r.Tools) > 0 {
			items = append(items, sayer(env, toneDim, "Tools: "+toolNames(r.Tools)))
		}
		items = append(items, buttons(env, s.rowActions(i, server)...))
		return stack(gtx, theme.Space1, items...)
	}
}

func toolNames(tools map[string]mcpTool) string {
	names := make([]string, 0, len(tools))
	for name := range tools {
		names = append(names, name)
	}
	slices.Sort(names)
	return strings.Join(names, ", ")
}

func (s *Servers) rowActions(i int, server mcpServer) []action {
	var out []action
	off := s.write.running() || s.editor.open
	if server.Editable {
		out = append(out,
			action{click: &s.edits.clicks[i], label: "Edit " + server.Name, variant: secondary, off: off},
			action{click: &s.removes.clicks[i], label: "Remove " + server.Name, variant: destructive, off: off})
	}
	if server.Effective && server.Config.Kind == "http" && server.Config.OAuth {
		out = append(out, action{click: &s.signIns.clicks[i], label: "Sign-in for " + server.Name, variant: secondary})
	}
	return out
}

func (s *Servers) editorWidgets(env scene) []layout.Widget {
	e := &s.editor
	off := s.write.running()
	title := "New user server"
	if e.existing != "" {
		title = "Edit user server " + e.existing
	}
	out := []layout.Widget{
		sayer(env, toneHeading, title),
		func(gtx layout.Context) layout.Dimensions {
			return e.name.layout(gtx, env, "Server name", off || e.existing != "")
		},
		func(gtx layout.Context) layout.Dimensions {
			return e.http.layout(gtx, env, "HTTP transport instead of stdio", off)
		},
	}
	if e.http.on {
		out = append(out, e.httpWidgets(env, off)...)
	} else {
		out = append(out, e.stdioWidgets(env, off)...)
	}
	for _, p := range e.problems {
		out = append(out, sayer(env, toneFailed, p))
	}
	return append(out, buttons(env,
		action{click: &e.save, label: "Save server", variant: pointed, off: off},
		action{click: &e.discard, label: "Discard edits", variant: quiet, off: off}))
}

func (e *serverEditor) httpWidgets(env scene, off bool) []layout.Widget {
	return []layout.Widget{
		func(gtx layout.Context) layout.Dimensions { return e.url.layout(gtx, env, "Server URL", off) },
		func(gtx layout.Context) layout.Dimensions {
			return e.bearer.layout(gtx, env, "Bearer token environment variable (optional)", off)
		},
		func(gtx layout.Context) layout.Dimensions { return e.oauth.layout(gtx, env, "Use OAuth sign-in", off) },
		func(gtx layout.Context) layout.Dimensions {
			return e.scopes.layout(gtx, env, "OAuth scopes (space separated)", off)
		},
		func(gtx layout.Context) layout.Dimensions {
			return e.clientID.layout(gtx, env, "OAuth client identifier (optional)", off)
		},
		func(gtx layout.Context) layout.Dimensions {
			return e.port.layout(gtx, env, "OAuth callback port (optional)", off)
		},
	}
}

func (e *serverEditor) stdioWidgets(env scene, off bool) []layout.Widget {
	out := []layout.Widget{
		func(gtx layout.Context) layout.Dimensions { return e.command.layout(gtx, env, "Command", off) },
		func(gtx layout.Context) layout.Dimensions {
			return e.args.layout(gtx, env, "Arguments, one per line", off)
		},
		sayer(env, toneDim, "Environment values stay hidden. A hidden value is kept unless you replace it."),
	}
	for _, entry := range e.env {
		out = append(out, entry.widget(env, off))
	}
	return append(out,
		func(gtx layout.Context) layout.Dimensions { return e.newKey.layout(gtx, env, "New variable name", off) },
		func(gtx layout.Context) layout.Dimensions {
			return e.newValue.layout(gtx, env, "New variable value", off)
		},
		buttons(env, action{click: &e.addEnv, label: "Add variable", variant: secondary, off: off}))
}

func (entry *envEntry) widget(env scene, off bool) layout.Widget {
	return func(gtx layout.Context) layout.Dimensions {
		state := "value set"
		if entry.redacted && !entry.replace.on {
			state = "value hidden and kept"
		}
		items := []layout.Widget{sayer(env, toneCode, entry.key+" · "+state)}
		if entry.redacted {
			items = append(items, func(gtx layout.Context) layout.Dimensions {
				return entry.replace.layout(gtx, env, "Replace the value of "+entry.key, off || entry.drop.on)
			})
		}
		if !entry.redacted || entry.replace.on {
			items = append(items, func(gtx layout.Context) layout.Dimensions {
				return entry.value.layout(gtx, env, "Value of "+entry.key, off || entry.drop.on)
			})
		}
		items = append(items, func(gtx layout.Context) layout.Dimensions {
			return entry.drop.layout(gtx, env, "Remove "+entry.key, off)
		})
		return stack(gtx, theme.Space1, items...)
	}
}
