package battery

import (
	"errors"
	"maps"
	"slices"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dalgona/discovery"
)

var errMCPNotLoaded = errors.New("battery: MCP servers are not loaded")

func (d *Daemon) ReloadUserMCP() ([]string, error) {
	p := d.mcp.Load()
	if p == nil {
		return nil, errMCPNotLoaded
	}
	user, err := config.LoadUserMCPServers(p.home)
	if err != nil {
		return nil, err
	}
	return p.reloadUser(nativeServers(user.Servers)), nil
}

func (p *projects) userServers() []discovery.MCPServer {
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.nativeList
}

func (p *projects) reloadUser(next []discovery.MCPServer) []string {
	p.mu.Lock()
	if p.closed {
		p.mu.Unlock()
		return nil
	}
	p.nativeList = next
	cwds := slices.Collect(maps.Keys(p.byCwd))
	p.mu.Unlock()
	starts, changed := p.native.retain(next)
	for _, server := range starts {
		p.native.start(server)
	}
	var restart []string
	for _, cwd := range cwds {
		missing, stopped := p.realign(cwd)
		restart = append(restart, missing...)
		changed = changed || stopped
	}
	slices.Sort(restart)
	if changed || len(starts) > 0 {
		p.toolEpoch.Add(1)
	}
	return slices.Compact(restart)
}

func (p *projects) realign(cwd string) ([]string, bool) {
	p.mu.Lock()
	group := p.byCwd[cwd]
	p.mu.Unlock()
	if group == nil {
		return nil, false
	}
	found, err := p.discover(cwd)
	if err != nil {
		p.log.Warn("mcp: discovery failed", "cwd", cwd, "error", err)
		return nil, false
	}
	kept := stripNative(found.MCPServers, p.userServers())
	home, workspace := splitWorkspace(kept)
	allowed := home
	p.mu.Lock()
	if p.workspaceAllowed(cwd, workspace) {
		allowed = kept
	}
	p.mu.Unlock()
	missing, stopped := group.retain(allowed)
	names := make([]string, 0, len(missing))
	for _, server := range missing {
		names = append(names, server.Name)
	}
	return names, stopped
}

func (s *servers) retain(wanted []discovery.MCPServer) ([]discovery.MCPServer, bool) {
	digests := make(map[string]string, len(wanted))
	for _, server := range wanted {
		digests[server.Name] = configDigest(server)
	}
	stopped := false
	for _, run := range s.pending() {
		if digests[run.spec.Name] != configDigest(run.spec) {
			s.stop(run)
			stopped = true
		}
	}
	return slices.DeleteFunc(slices.Clone(wanted), s.running), stopped
}

func (s *servers) running(server discovery.MCPServer) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	_, ok := s.runs[server.Name]
	return ok
}

func (s *servers) stop(run *serverRun) {
	s.mu.Lock()
	if s.runs[run.spec.Name] != run {
		s.mu.Unlock()
		return
	}
	delete(s.runs, run.spec.Name)
	s.order = slices.DeleteFunc(s.order, func(name string) bool { return name == run.spec.Name })
	s.mu.Unlock()
	run.cancel()
	<-run.done
	s.discard(run.client)
}
