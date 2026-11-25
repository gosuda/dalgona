package battery

import (
	"context"
	"crypto/hmac"
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"strings"
	"sync"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dalgona/discovery"
)

const (
	sourceUser       = "user"
	sourceProject    = "project"
	sourceDiscovered = "discovered"
)

var digestKey = sync.OnceValue(func() []byte { return []byte(rand.Text()) })

type inventoryBuilder struct {
	inventory agent.MCPInventory
	seen      map[string]bool
}

func userSource(discovery.MCPServer) string { return sourceUser }

func discoveredSource(server discovery.MCPServer) string {
	if server.Workspace {
		return sourceProject
	}
	return sourceDiscovered
}

func (d *Daemon) MCPConfigSnapshot(cwd string) (agent.MCPInventory, config.UserMCPServers, error) {
	p := d.mcp.Load()
	if p == nil {
		return agent.MCPInventory{}, config.UserMCPServers{}, errMCPNotLoaded
	}
	return p.inventorySnapshot(cwd)
}

func (p *projects) inventory(_ context.Context, cwd string) (agent.MCPInventory, error) {
	inventory, _, err := p.inventorySnapshot(cwd)
	return inventory, err
}

func (p *projects) inventorySnapshot(cwd string) (agent.MCPInventory, config.UserMCPServers, error) {
	cwd = config.CanonicalWorkspace(cwd)
	user, err := config.LoadUserMCPServers(p.home)
	if err != nil {
		return agent.MCPInventory{}, config.UserMCPServers{}, err
	}
	found, err := discovery.MCPCandidates(p.userHome, cwd, p.cfg.Discovery.Sources)
	if err != nil {
		return agent.MCPInventory{}, config.UserMCPServers{}, err
	}
	p.mu.Lock()
	trusted := p.trusted[cwd]
	p.mu.Unlock()
	builder := inventoryBuilder{inventory: agent.MCPInventory{TrustedWorkspace: trusted}, seen: map[string]bool{}}
	for _, server := range nativeServers(user.Servers) {
		builder.add(server, sourceUser)
	}
	for _, server := range found {
		builder.add(server, discoveredSource(server))
	}
	builder.inventory.Version = inventoryVersion(builder.inventory)
	return builder.inventory, user, nil
}

func (b *inventoryBuilder) add(server discovery.MCPServer, source string) {
	b.inventory.Servers = append(b.inventory.Servers, agent.MCPInventoryServer{
		Name: server.Name, Source: source, Effective: !b.seen[server.Name], ConfigDigest: configDigest(server), Config: serverSpec(server),
	})
	b.seen[server.Name] = true
}

func serverSpec(server discovery.MCPServer) config.MCPServerSpec {
	env := make(map[string]string, len(server.Env))
	for _, entry := range server.Env {
		key, value, _ := strings.Cut(entry, "=")
		env[key] = value
	}
	return config.MCPServerSpec{
		Command: server.Command, Args: server.Args, Env: env, URL: server.URL, BearerTokenEnv: server.BearerTokenEnv,
		OAuth: server.OAuth, OAuthScopes: server.OAuthScopes, OAuthClientID: server.OAuthClientID, OAuthCallbackPort: server.OAuthCallbackPort,
	}
}

func configDigest(server discovery.MCPServer) string {
	mac := hmac.New(sha256.New, digestKey())
	fmt.Fprintf(mac, "%q %q %q %q %q %t %q %q %d", server.Command, server.Args, server.Env, server.URL,
		server.BearerTokenEnv, server.OAuth, server.OAuthScopes, server.OAuthClientID, server.OAuthCallbackPort)
	return hex.EncodeToString(mac.Sum(nil))
}

func inventoryVersion(inventory agent.MCPInventory) string {
	digest := sha256.New()
	fmt.Fprintf(digest, "trusted %t\n", inventory.TrustedWorkspace)
	for _, server := range inventory.Servers {
		fmt.Fprintf(digest, "%q %s %t %s\n", server.Name, server.Source, server.Effective, server.ConfigDigest)
	}
	return hex.EncodeToString(digest.Sum(nil))
}
