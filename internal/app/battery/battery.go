package battery

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"log/slog"
	"maps"
	"os"
	"slices"
	"strings"
	"sync"
	"time"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/agent/prompt"
	"github.com/gosuda/dalgona/internal/app"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/discovery"
	"github.com/gosuda/dalgona/internal/mcp"
	"github.com/gosuda/dalgona/internal/remote"
	"github.com/gosuda/dalgona/internal/script"
	"github.com/gosuda/dalgona/internal/scripthost"
	"github.com/gosuda/dalgona/internal/skills"
	"github.com/gosuda/dalgona/internal/tool"
)

var mcpStartTimeout = 30 * time.Second

type Daemon struct {
	Remote            remote.Options
	Scripts           scripthost.Config
	WorkspaceApprover WorkspaceApprover
	binder            *remote.Binder
}

type TrustDecision int

const (
	TrustRefuse TrustDecision = iota
	TrustAccept
	TrustCancel
)

type WorkspaceApprover func(ctx context.Context, cwd string, servers []discovery.MCPServer) TrustDecision

func (d *Daemon) Load(ctx context.Context, base app.Base) (app.Batteries, error) {
	logger := cmp.Or(base.Logger, slog.Default())
	userHome, err := os.UserHomeDir()
	if err != nil {
		return app.Batteries{}, fmt.Errorf("user home: %w", err)
	}
	plugins, err := script.LoadPlugins(base.Config.Home)
	if err != nil {
		logger.Error("plugins: some files did not load", "error", err)
	}
	life := context.WithoutCancel(ctx)
	native := nativeServers(base.Config)
	projects := &projects{
		cfg: base.Config, home: base.Config.Home, userHome: userHome, log: logger, life: life, nativeList: native,
		native: startServers(life, native, logger), byCwd: make(map[string]*servers),
		approver: d.WorkspaceApprover, trusted: map[string]bool{}, refused: map[string]bool{}, pending: map[string]chan struct{}{},
	}
	projects.trusted = loadTrusted(base.Config.Home, logger)
	d.Remote = remote.Options{Config: base.Config, Auth: base.Auth, Journal: base.Journal, Local: base.Local, Logger: logger}
	d.binder = remote.NewBinder(d.Remote)
	d.Scripts = scripthost.Config{
		Plugins: plugins, Judge: base.Judge, Mode: base.Config.ToolMode, Eval: base.Config.Eval, Logger: logger,
	}
	return app.Batteries{
		Tools: func(ctx context.Context, cwd string) []tool.Tool {
			return append(projects.tools(ctx, cwd), ratchetTools(ctx)...)
		},
		Skills: projects.skills, Runner: d.binder.Runner,
		Extension: scripthost.New(d.Scripts),
		Attach:    func(h *agent.Host) { d.binder.SetRecorder(h) }, Close: projects.close,
		MCPStatus: projects.status,
	}, nil
}

func (d *Daemon) Archive(ctx context.Context, threadID string) error {
	return d.binder.Archive(ctx, threadID)
}

type projects struct {
	cfg        config.Config
	home       string
	userHome   string
	log        *slog.Logger
	life       context.Context
	nativeList []discovery.MCPServer
	native     *servers
	approver   WorkspaceApprover
	mu         sync.Mutex
	closed     bool
	byCwd      map[string]*servers
	trusted    map[string]bool
	refused    map[string]bool
	pending    map[string]chan struct{}
}

func (p *projects) discover(cwd string) (discovery.Result, error) {
	listed, err := skills.List(p.cfg.Home, cwd, nil)
	if err != nil {
		return discovery.Result{}, fmt.Errorf("skills: %w", err)
	}
	native := discovery.Native{Skills: make([]discovery.Skill, 0, len(listed))}
	for _, skill := range listed {
		native.Skills = append(native.Skills, discovery.Skill(skill))
	}
	native.MCPServers = p.nativeList
	return discovery.Discover(p.userHome, cwd, p.cfg.Discovery.Sources, native)
}

func nativeServers(cfg config.Config) []discovery.MCPServer {
	out := make([]discovery.MCPServer, 0, len(cfg.MCPServers))
	for _, name := range slices.Sorted(maps.Keys(cfg.MCPServers)) {
		out = append(out, nativeServer(name, cfg.MCPServers[name]))
	}
	return out
}

func (p *projects) skills(cwd string) ([]prompt.Skill, error) {
	found, err := p.discover(cwd)
	if err != nil {
		return nil, err
	}
	out := make([]prompt.Skill, 0, len(found.Skills))
	for _, skill := range found.Skills {
		out = append(out, prompt.Skill(skill))
	}
	return out, nil
}

func (p *projects) tools(ctx context.Context, cwd string) []tool.Tool {
	return append(p.native.tools(), p.serversFor(ctx, config.CanonicalWorkspace(cwd)).tools()...)
}

func (p *projects) status(_ context.Context, cwd string) []agent.MCPServerStatus {
	cwd = config.CanonicalWorkspace(cwd)
	p.mu.Lock()
	workspace := p.byCwd[cwd]
	p.mu.Unlock()
	out := p.native.statuses()
	if workspace != nil {
		out = append(out, workspace.statuses()...)
	}
	slices.SortStableFunc(out, func(a, b agent.MCPServerStatus) int { return cmp.Compare(a.Name, b.Name) })
	return out
}

func (p *projects) serversFor(ctx context.Context, cwd string) *servers {
	cwd = config.CanonicalWorkspace(cwd)
	for {
		started, wait, owned := p.begin(cwd)
		if started != nil {
			return started
		}
		if !owned {
			select {
			case <-wait:
				continue
			case <-ctx.Done():
				return &servers{}
			case <-p.life.Done():
				return &servers{}
			}
		}
		return p.resolveOwned(ctx, cwd)
	}
}

func (p *projects) begin(cwd string) (started *servers, wait <-chan struct{}, owned bool) {
	cwd = config.CanonicalWorkspace(cwd)
	p.mu.Lock()
	defer p.mu.Unlock()
	if cached, ok := p.byCwd[cwd]; ok {
		return cached, nil, false
	}
	if p.closed {
		return &servers{}, nil, false
	}
	if pending, ok := p.pending[cwd]; ok {
		return nil, pending, false
	}
	p.pending[cwd] = make(chan struct{})
	return nil, nil, true
}

func (p *projects) resolveOwned(ctx context.Context, cwd string) *servers {
	cwd = config.CanonicalWorkspace(cwd)
	found, err := p.discover(cwd)
	if err != nil {
		p.log.Warn("mcp: discovery failed", "cwd", cwd, "error", err)
	}
	kept := stripNative(found.MCPServers, p.nativeList)
	home, workspace := splitWorkspace(kept)
	allowed, prompt := p.classify(cwd, home, kept, workspace)
	cache := true
	if prompt {
		allowed, cache = p.settleTrust(ctx, cwd, home, kept, workspace)
	}
	if !cache {
		p.mu.Lock()
		done := p.pending[cwd]
		delete(p.pending, cwd)
		p.mu.Unlock()
		close(done)
		return &servers{}
	}
	started := startServers(p.life, allowed, p.log)
	p.mu.Lock()
	done := p.pending[cwd]
	delete(p.pending, cwd)
	if p.closed {
		p.mu.Unlock()
		_ = started.close()
		close(done)
		return &servers{}
	}
	p.byCwd[cwd] = started
	p.mu.Unlock()
	close(done)
	return started
}

func (p *projects) classify(cwd string, home, kept, workspace []discovery.MCPServer) ([]discovery.MCPServer, bool) {
	cwd = config.CanonicalWorkspace(cwd)
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.workspaceAllowed(cwd, workspace) {
		return kept, false
	}
	if p.refused[cwd] {
		return home, false
	}
	return home, true
}

func (p *projects) settleTrust(ctx context.Context, cwd string, home, kept, workspace []discovery.MCPServer) ([]discovery.MCPServer, bool) {
	cwd = config.CanonicalWorkspace(cwd)
	switch p.requestTrust(ctx, cwd, workspace) {
	case TrustAccept:
		p.noteTrusted(cwd)
		return kept, true
	case TrustCancel:
		return nil, false
	case TrustRefuse:
		p.noteRefused(cwd)
		return home, true
	default:
		p.noteRefused(cwd)
		return home, true
	}
}

func (p *projects) workspaceAllowed(cwd string, workspace []discovery.MCPServer) bool {
	return len(workspace) == 0 || p.trusted[config.CanonicalWorkspace(cwd)]
}

func (p *projects) requestTrust(ctx context.Context, cwd string, workspace []discovery.MCPServer) TrustDecision {
	if p.approver == nil {
		return TrustRefuse
	}
	return p.approver(ctx, config.CanonicalWorkspace(cwd), workspace)
}

func (p *projects) noteRefused(cwd string) {
	cwd = config.CanonicalWorkspace(cwd)
	p.mu.Lock()
	defer p.mu.Unlock()
	p.refused[cwd] = true
}

func (p *projects) noteTrusted(cwd string) {
	cwd = config.CanonicalWorkspace(cwd)
	if err := config.TrustWorkspace(p.home, cwd); err != nil {
		p.log.Warn("mcp: trust workspace not persisted", "cwd", cwd, "error", err)
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	p.trusted[cwd] = true
	delete(p.refused, cwd)
}

func loadTrusted(home string, logger *slog.Logger) map[string]bool {
	trusted := map[string]bool{}
	listed, err := config.TrustedWorkspaces(home)
	if err != nil {
		logger.Warn("mcp: trusted workspaces unreadable", "error", err)
		return trusted
	}
	for _, cwd := range listed {
		if cwd != "" {
			trusted[config.CanonicalWorkspace(cwd)] = true
		}
	}
	return trusted
}

func stripNative(found []discovery.MCPServer, native []discovery.MCPServer) []discovery.MCPServer {
	return slices.DeleteFunc(slices.Clone(found), func(server discovery.MCPServer) bool {
		return slices.ContainsFunc(native, func(other discovery.MCPServer) bool { return other.Name == server.Name })
	})
}

func splitWorkspace(found []discovery.MCPServer) (home, workspace []discovery.MCPServer) {
	for _, server := range found {
		if server.Workspace {
			workspace = append(workspace, server)
		} else {
			home = append(home, server)
		}
	}
	return home, workspace
}

func WorkspaceTrustPrompt(cwd string, servers []discovery.MCPServer) string {
	ordered := slices.Clone(servers)
	slices.SortFunc(ordered, func(a, b discovery.MCPServer) int { return strings.Compare(a.Name, b.Name) })
	lines := make([]string, 0, len(servers)+1)
	lines = append(lines, "Trust MCP servers for workspace "+cwd+"?")
	for _, server := range ordered {
		lines = append(lines, describeWorkspaceServer(server))
	}
	return strings.Join(lines, "\n")
}

func describeWorkspaceServer(server discovery.MCPServer) string {
	if server.URL != "" {
		base := server.Name + ": " + server.URL
		if len(server.Env) > 0 {
			env := slices.Clone(server.Env)
			slices.Sort(env)
			base += " env " + strings.Join(env, " ")
		}
		return base
	}
	detail := strings.TrimSpace(server.Command + " " + strings.Join(server.Args, " "))
	if len(server.Env) > 0 {
		env := slices.Clone(server.Env)
		slices.Sort(env)
		envText := "env " + strings.Join(env, " ")
		if detail == "" {
			detail = envText
		} else {
			detail += " " + envText
		}
	}
	if detail == "" {
		return server.Name
	}
	return server.Name + ": " + detail
}

func (p *projects) close() error {
	p.mu.Lock()
	p.closed = true
	all := append(slices.Collect(maps.Values(p.byCwd)), p.native)
	clear(p.byCwd)
	p.mu.Unlock()
	errs := make([]error, 0, len(all))
	for _, started := range all {
		errs = append(errs, started.close())
	}
	return errors.Join(errs...)
}

func nativeServer(name string, spec config.MCPServerSpec) discovery.MCPServer {
	env := make([]string, 0, len(spec.Env))
	for _, key := range slices.Sorted(maps.Keys(spec.Env)) {
		env = append(env, key+"="+spec.Env[key])
	}
	return discovery.MCPServer{Name: name, Command: spec.Command, Args: spec.Args, Env: env, URL: spec.URL}
}

type serverState struct {
	Status string
	Error  string
}

type servers struct {
	cancel  context.CancelFunc
	started sync.WaitGroup
	mu      sync.Mutex
	clients []*mcp.Client
	byName  map[string][]tool.Tool
	order   []string
	states  map[string]serverState
}

func startServers(ctx context.Context, found []discovery.MCPServer, logger *slog.Logger) *servers {
	ctx, cancel := context.WithCancel(ctx)
	s := &servers{cancel: cancel, byName: make(map[string][]tool.Tool, len(found)), states: make(map[string]serverState, len(found))}
	for _, server := range found {
		s.order = append(s.order, server.Name)
		s.states[server.Name] = serverState{Status: "starting"}
		s.started.Go(func() {
			client, tools, err := connect(ctx, server)
			s.mu.Lock()
			defer s.mu.Unlock()
			if err != nil {
				logger.Warn("mcp: server skipped", "server", server.Name, "error", err)
				s.states[server.Name] = serverState{Status: "failed", Error: err.Error()}
				return
			}
			s.clients = append(s.clients, client)
			s.byName[server.Name] = tools
			s.states[server.Name] = serverState{Status: "connected"}
		})
	}
	return s
}

func (s *servers) tools() []tool.Tool {
	s.started.Wait()
	s.mu.Lock()
	defer s.mu.Unlock()
	var out []tool.Tool
	for _, name := range s.order {
		out = append(out, s.byName[name]...)
	}
	return out
}

func (s *servers) statuses() []agent.MCPServerStatus {
	s.mu.Lock()
	defer s.mu.Unlock()
	out := make([]agent.MCPServerStatus, 0, len(s.states))
	for _, name := range slices.Sorted(maps.Keys(s.states)) {
		state := s.states[name]
		status := agent.MCPServerStatus{Name: name, Status: state.Status, Error: state.Error}
		for _, t := range s.byName[name] {
			status.Tools = append(status.Tools, agent.MCPToolStatus{Name: mcp.LocalToolName(t), Spec: t.Spec()})
		}
		out = append(out, status)
	}
	return out
}

func (s *servers) close() error {
	if s.cancel != nil {
		s.cancel()
	}
	s.started.Wait()
	s.mu.Lock()
	defer s.mu.Unlock()
	errs := make([]error, 0, len(s.clients))
	for _, client := range s.clients {
		errs = append(errs, client.Close())
	}
	return errors.Join(errs...)
}

func connect(ctx context.Context, server discovery.MCPServer) (*mcp.Client, []tool.Tool, error) {
	ctx, cancel := context.WithTimeout(ctx, mcpStartTimeout)
	defer cancel()
	client, err := mcp.New(ctx, mcp.Config{Name: server.Name, Command: server.Command, Args: server.Args, Env: server.Env, URL: server.URL})
	if err != nil {
		return nil, nil, err
	}
	tools, err := client.Tools(ctx)
	if err != nil {
		return nil, nil, errors.Join(err, client.Close())
	}
	return client, tools, nil
}
