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
	Remote  remote.Options
	Scripts scripthost.Config
	binder  *remote.Binder
}

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
		cfg: base.Config, userHome: userHome, log: logger, life: life, nativeList: native,
		native: startServers(life, native, logger), byCwd: make(map[string]*servers),
	}
	d.Remote = remote.Options{Config: base.Config, Auth: base.Auth, Journal: base.Journal, Local: base.Local, Logger: logger}
	d.binder = remote.NewBinder(d.Remote)
	d.Scripts = scripthost.Config{
		Plugins: plugins, Judge: base.Judge, Mode: base.Config.ToolMode, Eval: base.Config.Eval, Logger: logger,
	}
	return app.Batteries{
		Tools: projects.tools, Skills: projects.skills, Runner: d.binder.Runner,
		Extension: scripthost.New(d.Scripts),
		Attach:    func(h *agent.Host) { d.binder.SetRecorder(h) }, Close: projects.close,
	}, nil
}

func (d *Daemon) Archive(ctx context.Context, threadID string) error {
	return d.binder.Archive(ctx, threadID)
}

type projects struct {
	cfg        config.Config
	userHome   string
	log        *slog.Logger
	life       context.Context
	nativeList []discovery.MCPServer
	native     *servers
	mu         sync.Mutex
	closed     bool
	byCwd      map[string]*servers
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

func (p *projects) tools(cwd string) []tool.Tool {
	return append(p.native.tools(), p.serversFor(cwd).tools()...)
}

func (p *projects) serversFor(cwd string) *servers {
	p.mu.Lock()
	defer p.mu.Unlock()
	if started, ok := p.byCwd[cwd]; ok {
		return started
	}
	if p.closed {
		return &servers{}
	}
	found, err := p.discover(cwd)
	if err != nil {
		p.log.Warn("mcp: discovery failed", "cwd", cwd, "error", err)
	}
	discovered := slices.DeleteFunc(found.MCPServers, func(server discovery.MCPServer) bool {
		return slices.ContainsFunc(p.nativeList, func(native discovery.MCPServer) bool { return native.Name == server.Name })
	})
	started := startServers(p.life, discovered, p.log)
	p.byCwd[cwd] = started
	return started
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

type servers struct {
	cancel  context.CancelFunc
	started sync.WaitGroup
	mu      sync.Mutex
	clients []*mcp.Client
	byName  map[string][]tool.Tool
	order   []string
}

func startServers(ctx context.Context, found []discovery.MCPServer, logger *slog.Logger) *servers {
	ctx, cancel := context.WithCancel(ctx)
	s := &servers{cancel: cancel, byName: make(map[string][]tool.Tool, len(found))}
	for _, server := range found {
		s.order = append(s.order, server.Name)
		s.started.Go(func() {
			client, tools, err := connect(ctx, server)
			if err != nil {
				logger.Warn("mcp: server skipped", "server", server.Name, "error", err)
				return
			}
			s.mu.Lock()
			defer s.mu.Unlock()
			s.clients = append(s.clients, client)
			s.byName[server.Name] = tools
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
