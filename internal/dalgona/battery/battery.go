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
	"sync/atomic"
	"time"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/agent/prompt"
	"github.com/gosuda/dalgona/internal/dal/app"
	"github.com/gosuda/dalgona/internal/dal/auth"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/failure"
	corescripthost "github.com/gosuda/dalgona/internal/dal/scripthost"
	"github.com/gosuda/dalgona/internal/dal/tool"
	"github.com/gosuda/dalgona/internal/dalgona/daltoki"
	"github.com/gosuda/dalgona/internal/dalgona/discovery"
	"github.com/gosuda/dalgona/internal/dalgona/github"
	"github.com/gosuda/dalgona/internal/dalgona/mcp"
	"github.com/gosuda/dalgona/internal/dalgona/postbox"
	"github.com/gosuda/dalgona/internal/dalgona/remote"
	"github.com/gosuda/dalgona/internal/dalgona/report"
	batteryscript "github.com/gosuda/dalgona/internal/dalgona/script"
	batteryscripthost "github.com/gosuda/dalgona/internal/dalgona/scripthost"
)

var mcpStartTimeout = 30 * time.Second

type Daemon struct {
	Remote            remote.Options
	Scripts           batteryscripthost.Config
	WorkspaceApprover WorkspaceApprover
	ReporterOpener    func(ctx context.Context, home string) (*report.Reporter, error)
	binder            *remote.Binder
	daltoki           *daltoki.Service
	postboxSvc        atomic.Pointer[postbox.Service]
	postboxCancel     context.CancelFunc
	postboxMu         sync.Mutex
	reportMu          sync.Mutex
	reportHome        string
	reportLogger      *slog.Logger
	reporter          *report.Reporter
	mcp               atomic.Pointer[projects]
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
	plugins, err := batteryscript.LoadPlugins(base.Config.Home)
	if err != nil {
		logger.Error("plugins: some files did not load", "error", err)
	}
	life := context.WithoutCancel(ctx)
	native := nativeServers(base.Config.MCPServers)
	creds := mcpAuth{home: base.Config.Home, store: base.Auth}
	projects := &projects{
		cfg: base.Config, home: base.Config.Home, userHome: userHome, log: logger, life: life, nativeList: native,
		creds: creds, native: startServers(life, native, logger, creds, userSource), byCwd: make(map[string]*servers),
		approver: d.WorkspaceApprover, trusted: map[string]bool{}, refused: map[string]bool{}, pending: map[string]chan struct{}{},
	}
	projects.trusted = loadTrusted(base.Config.Home, logger)
	d.mcp.Store(projects)
	d.Remote = remote.Options{Config: base.Config, Auth: base.Auth, Journal: base.Journal, Local: base.Local, Logger: logger}
	d.binder = remote.NewBinder(d.Remote)
	d.openReporter(ctx, base.Config.Home, logger)
	d.Scripts = batteryscripthost.Config{
		Core:    corescripthost.Config{Judge: base.Judge, Mode: base.Config.ToolMode, Eval: base.Config.Eval, Logger: logger},
		Plugins: plugins,
	}
	daltokiSvc, err := loadDaltoki(ctx, base, logger)
	if err != nil {
		return app.Batteries{}, err
	}
	d.daltoki = daltokiSvc
	return app.Batteries{
		Tools: func(ctx context.Context, cwd string) []tool.Tool {
			out := append(projects.tools(ctx, cwd), ratchetTools(ctx)...)
			return append(out, postbox.Tools(ctx, d.postboxSvc.Load())...)
		},
		ToolsGeneration: projects.toolEpoch.Load,
		Skills:          projects.skills, Runner: d.binder.Runner,
		Extension: batteryscripthost.New(d.Scripts),
		Attach: func(h *agent.Host) {
			d.binder.SetRecorder(h)
			if daltokiSvc != nil {
				daltokiSvc.Attach(h)
			}
			d.attachPostbox(h)
		},
		Close: d.closeWithPostbox(func() error {
			var closeErr error
			if daltokiSvc != nil {
				closeCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
				closeErr = daltokiSvc.Close(closeCtx)
				cancel()
			}
			return errors.Join(closeErr, projects.close(), d.closeReporter())
		}),
		MCPStatus:    projects.status,
		MCPInventory: projects.inventory,
		FailureSink: func(record failure.Record) {
			if err := d.ReportFailure(life, record); err != nil {
				logger.Error("failure report not queued", "error", err)
			}
		},
	}, nil
}

func loadDaltoki(ctx context.Context, base app.Base, logger *slog.Logger) (*daltoki.Service, error) {
	cfg, err := daltoki.ParseConfig(base.Config.Daltoki, base.Auth.UnmaskStrict)
	if err != nil {
		return nil, err
	}
	if !cfg.Enabled {
		return nil, nil
	}
	repositories := make([]github.Repository, 0, len(cfg.Repositories))
	repositories = append(repositories, cfg.Repositories...)
	cred, err := github.NewInstallationCredential(ctx, github.InstallationConfig{
		AppID:          cfg.AppID,
		PrivateKeyFile: cfg.PrivateKeyFile,
		Repositories:   repositories,
	}, nil)
	if err != nil {
		return nil, err
	}
	if maskErr := cred.RegisterSecrets(base.Auth.MaskWithError); maskErr != nil {
		return nil, maskErr
	}
	client, err := github.NewClient(github.ClientConfig{
		Repositories: repositories,
		Policy:       "bot",
	}, cred, nil)
	if err != nil {
		return nil, err
	}
	return daltoki.New(ctx, cfg, daltoki.Dependencies{
		Home:      base.Config.Home,
		Client:    client,
		Graphs:    daltoki.NewScheduler(daltoki.HostNodes{}),
		Worktrees: daltoki.NewWorktrees(base.Config.Home, "https://github.com/", cred),
		Auth:      base.Auth,
		Clock:     time.Now,
		Logger:    logger,
	})
}

func (d *Daemon) AfterRecover(ctx context.Context, host *agent.Host, recoverErr error) {
	if d.daltoki == nil {
		return
	}
	d.daltoki.StartAfterRecovery(recoverErr)
	if recoverErr != nil {
		return
	}
	if err := d.daltoki.Run(ctx); err != nil && ctx.Err() == nil {
		slog.Error("daltoki: service stopped", "error", err)
	}
}

func (d *Daemon) attachPostbox(h *agent.Host) {
	d.postboxMu.Lock()
	defer d.postboxMu.Unlock()
	if d.postboxCancel != nil {
		d.postboxCancel()
		d.postboxCancel = nil
	}
	ctx, cancel := context.WithCancel(context.Background())
	service, err := postbox.New(ctx, postbox.Options{
		Host: h, Sessions: true, UserID: daemonUID(),
		ContinueForMail: func(ctx context.Context, threadID string) error {
			thread, err := h.Resume(ctx, threadID)
			if err != nil {
				return err
			}
			return thread.RunMailTurn(ctx)
		},
	})
	if err != nil {
		cancel()
		return
	}
	d.postboxCancel = cancel
	d.postboxSvc.Store(service)
}

func (d *Daemon) closeWithPostbox(next func() error) func() error {
	return func() error {
		d.postboxMu.Lock()
		cancel := d.postboxCancel
		d.postboxCancel = nil
		service := d.postboxSvc.Swap(nil)
		d.postboxMu.Unlock()
		if cancel != nil {
			cancel()
		}
		err := next()
		if service != nil {
			ctx, stop := context.WithTimeout(context.Background(), 5*time.Second)
			defer stop()
			err = errors.Join(err, service.Close(ctx))
		}
		return err
	}
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
	toolEpoch  atomic.Uint64
	approver   WorkspaceApprover
	mu         sync.Mutex
	closed     bool
	byCwd      map[string]*servers
	trusted    map[string]bool
	refused    map[string]bool
	pending    map[string]chan struct{}
	creds      mcpAuth
}

func (p *projects) discover(cwd string) (discovery.Result, error) {
	listed, err := discovery.NativeSkills(p.cfg.Home, cwd, nil)
	if err != nil {
		return discovery.Result{}, fmt.Errorf("skills: %w", err)
	}
	native := discovery.Result{Skills: listed, MCPServers: p.userServers()}
	return discovery.Discover(p.userHome, cwd, p.cfg.Discovery.Sources, native)
}

func nativeServers(specs map[string]config.MCPServerSpec) []discovery.MCPServer {
	out := make([]discovery.MCPServer, 0, len(specs))
	for _, name := range slices.Sorted(maps.Keys(specs)) {
		out = append(out, nativeServer(name, specs[name]))
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
	kept := stripNative(found.MCPServers, p.userServers())
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
	started := startServers(p.life, allowed, p.log, p.creds, discoveredSource)
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
	return discovery.MCPServer{
		Name: name, Command: spec.Command, Args: spec.Args, Env: env, URL: spec.URL,
		BearerTokenEnv: spec.BearerTokenEnv, OAuth: spec.OAuth, OAuthScopes: spec.OAuthScopes,
		OAuthClientID: spec.OAuthClientID, OAuthCallbackPort: spec.OAuthCallbackPort,
	}
}

type serverState struct {
	Status string
	Error  string
}

type mcpAuth struct {
	home  string
	store *auth.Store
}

func (a mcpAuth) options(server discovery.MCPServer) mcp.AuthOptions {
	return mcp.AuthOptions{
		Name: server.Name, URL: server.URL, BearerTokenEnv: server.BearerTokenEnv,
		OAuth: server.OAuth, OAuthClientID: server.OAuthClientID, Home: a.home, Store: a.store,
	}
}

func failedState(name string, err error) serverState {
	if _, ok := errors.AsType[*mcp.LoginRequiredError](err); ok {
		return serverState{Status: "authenticationRequired", Error: "login required: run dalgona mcp login " + name}
	}
	return serverState{Status: "failed", Error: err.Error()}
}

type serverRun struct {
	spec   discovery.MCPServer
	state  serverState
	client *mcp.Client
	tools  []tool.Tool
	cancel context.CancelFunc
	done   chan struct{}
}

type servers struct {
	life   context.Context
	cancel context.CancelFunc
	logger *slog.Logger
	creds  mcpAuth
	source func(discovery.MCPServer) string
	mu     sync.Mutex
	order  []string
	runs   map[string]*serverRun
}

func startServers(ctx context.Context, found []discovery.MCPServer, logger *slog.Logger, creds mcpAuth, source func(discovery.MCPServer) string) *servers {
	life, cancel := context.WithCancel(ctx)
	s := &servers{life: life, cancel: cancel, logger: logger, creds: creds, source: source, runs: make(map[string]*serverRun, len(found))}
	for _, server := range found {
		s.start(server)
	}
	return s
}

func (s *servers) start(server discovery.MCPServer) {
	ctx, cancel := context.WithCancel(s.life)
	run := &serverRun{spec: server, state: serverState{Status: "starting"}, cancel: cancel, done: make(chan struct{})}
	s.mu.Lock()
	s.order = append(s.order, server.Name)
	s.runs[server.Name] = run
	s.mu.Unlock()
	go func() {
		defer close(run.done)
		client, tools, err := connect(ctx, server, s.creds)
		s.discard(s.settle(run, client, tools, err))
	}()
}

func (s *servers) discard(client *mcp.Client) {
	if client == nil {
		return
	}
	if err := client.Close(); err != nil {
		s.logger.Warn("mcp: close a replaced transport", "error", err)
	}
}

func (s *servers) settle(run *serverRun, client *mcp.Client, tools []tool.Tool, err error) *mcp.Client {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.runs[run.spec.Name] != run {
		return client
	}
	if err != nil {
		s.logger.Warn("mcp: server skipped", "server", run.spec.Name, "error", err)
		run.state = failedState(run.spec.Name, err)
		return nil
	}
	run.client, run.tools, run.state = client, tools, serverState{Status: "connected"}
	return nil
}

func (s *servers) pending() []*serverRun {
	s.mu.Lock()
	defer s.mu.Unlock()
	return slices.Collect(maps.Values(s.runs))
}

func (s *servers) tools() []tool.Tool {
	for _, run := range s.pending() {
		<-run.done
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	var out []tool.Tool
	for _, name := range s.order {
		if run := s.runs[name]; run != nil {
			out = append(out, run.tools...)
		}
	}
	return out
}

func (s *servers) statuses() []agent.MCPServerStatus {
	s.mu.Lock()
	defer s.mu.Unlock()
	out := make([]agent.MCPServerStatus, 0, len(s.runs))
	for _, name := range slices.Sorted(maps.Keys(s.runs)) {
		run := s.runs[name]
		status := agent.MCPServerStatus{
			Name: name, Source: s.source(run.spec), ConfigDigest: configDigest(run.spec),
			AuthStatus: mcp.AuthStatus(s.creds.options(run.spec)), Status: run.state.Status, Error: run.state.Error,
		}
		for _, t := range run.tools {
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
	runs := s.pending()
	errs := make([]error, 0, len(runs))
	for _, run := range runs {
		<-run.done
		if run.client != nil {
			errs = append(errs, run.client.Close())
		}
	}
	return errors.Join(errs...)
}

func connect(ctx context.Context, server discovery.MCPServer, creds mcpAuth) (*mcp.Client, []tool.Tool, error) {
	ctx, cancel := context.WithTimeout(ctx, mcpStartTimeout)
	defer cancel()
	options := creds.options(server)
	if server.OAuth && mcp.AuthStatus(options) != mcp.AuthOAuth {
		return nil, nil, &mcp.LoginRequiredError{Name: server.Name}
	}
	source, err := mcp.NewTokenSource(options)
	if err != nil {
		return nil, nil, err
	}
	client, err := mcp.New(ctx, mcp.Config{Name: server.Name, Command: server.Command, Args: server.Args, Env: server.Env, URL: server.URL, Auth: source})
	if err != nil {
		return nil, nil, err
	}
	tools, err := client.Tools(ctx)
	if err != nil {
		return nil, nil, errors.Join(err, client.Close())
	}
	return client, tools, nil
}

func (d *Daemon) openReporter(ctx context.Context, home string, logger *slog.Logger) {
	if d.ReporterOpener == nil {
		return
	}
	d.reportHome = home
	d.reportLogger = logger
	opened, err := d.ReporterOpener(ctx, home)
	if err != nil {
		logger.Error("failure reporter unavailable", "error", err)
		return
	}
	d.reporter = opened
}

func (d *Daemon) closeReporter() error {
	d.reportMu.Lock()
	defer d.reportMu.Unlock()
	d.reportHome = ""
	return d.retireReporter()
}

func (d *Daemon) retireReporter() error {
	if d.reporter == nil {
		return nil
	}
	reporter := d.reporter
	d.reporter = nil
	return reporter.Close(context.Background())
}

func (d *Daemon) ReloadReporter() {
	d.reportMu.Lock()
	defer d.reportMu.Unlock()
	if d.reportHome == "" {
		return
	}
	if err := d.retireReporter(); err != nil {
		d.reportLogger.Error("failure reporter did not close", "error", err)
	}
	d.openReporter(context.Background(), d.reportHome, d.reportLogger)
}

func (d *Daemon) ReportFailure(ctx context.Context, record failure.Record) error {
	d.reportMu.Lock()
	defer d.reportMu.Unlock()
	if d.reporter == nil && d.reportHome != "" && report.ConsentStored(d.reportHome) {
		d.openReporter(context.Background(), d.reportHome, d.reportLogger)
	}
	if d.reporter == nil {
		return nil
	}
	return d.reporter.Enqueue(ctx, record)
}
