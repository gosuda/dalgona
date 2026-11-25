package main

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"slices"
	"strconv"
	"strings"
	"sync"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/rpcserver"
	"github.com/gosuda/dalgona/internal/dal/textinput"
	"github.com/gosuda/dalgona/internal/dalgona/battery"
)

const (
	mcpEditLimit       = 32
	mcpWriteBytes      = 256 << 10
	mcpIssuedLimit     = 256
	mcpNameBytes       = 256
	mcpSourceUser      = "user"
	mcpKindStdio       = "stdio"
	mcpKindHTTP        = "http"
	mcpActionPut       = "put"
	mcpActionRemove    = "remove"
	mcpServersSettings = "mcp_servers."
)

type (
	mcpEnvValue struct {
		Value    *string `json:"value,omitzero"`
		Preserve bool    `json:"preserve,omitzero"`
		Redacted bool    `json:"redacted,omitzero"`
	}
	mcpStdioConfig struct {
		Kind    string                 `json:"kind"`
		Command string                 `json:"command"`
		Args    []string               `json:"args"`
		Env     map[string]mcpEnvValue `json:"env"`
	}
	mcpHTTPConfig struct {
		Kind              string   `json:"kind"`
		URL               string   `json:"url"`
		BearerTokenEnv    string   `json:"bearerTokenEnv,omitzero"`
		OAuth             bool     `json:"oauth"`
		OAuthScopes       []string `json:"oauthScopes,omitzero"`
		OAuthClientID     string   `json:"oauthClientId,omitzero"`
		OAuthCallbackPort int      `json:"oauthCallbackPort,omitzero"`
	}
	mcpEditConfig struct {
		Kind              string                 `json:"kind"`
		Command           string                 `json:"command"`
		Args              []string               `json:"args"`
		Env               map[string]mcpEnvValue `json:"env"`
		URL               string                 `json:"url"`
		BearerTokenEnv    string                 `json:"bearerTokenEnv"`
		OAuth             bool                   `json:"oauth"`
		OAuthScopes       []string               `json:"oauthScopes"`
		OAuthClientID     string                 `json:"oauthClientId"`
		OAuthCallbackPort int                    `json:"oauthCallbackPort"`
	}
	mcpServerView struct {
		Name      string `json:"name"`
		Source    string `json:"source"`
		Editable  bool   `json:"editable"`
		Config    any    `json:"config"`
		Effective bool   `json:"effective"`
	}
	mcpConfigView struct {
		Version          string          `json:"version"`
		Servers          []mcpServerView `json:"servers"`
		TrustedWorkspace bool            `json:"trustedWorkspace"`
	}
	mcpEdit struct {
		Name   string         `json:"name"`
		Action string         `json:"action"`
		Config *mcpEditConfig `json:"config"`
	}
	mcpWriteParams struct {
		ExpectedVersion string    `json:"expectedVersion"`
		Edits           []mcpEdit `json:"edits"`
	}
	mcpReadParams struct {
		ThreadID string `json:"threadId"`
	}
)

type mcpConfigService struct {
	batteries *battery.Daemon
	write     sync.Mutex
	mu        sync.Mutex
	issued    map[string]string
	order     []string
}

type mcpConfigCall struct {
	service *mcpConfigService
	host    *agent.Host
}

func newMCPConfigService(batteries *battery.Daemon) *mcpConfigService {
	return &mcpConfigService{batteries: batteries, issued: map[string]string{}}
}

func (m *mcpConfigService) extensions(host *agent.Host) map[string]rpcserver.Extension {
	call := mcpConfigCall{service: m, host: host}
	return map[string]rpcserver.Extension{"dalgona/mcp/config/read": rpcserver.WithDeadline(rpcserver.PageDeadline, call.read)}
}

func (m *mcpConfigService) operations(host *agent.Host) map[string]rpcserver.Extension {
	call := mcpConfigCall{service: m, host: host}
	return map[string]rpcserver.Extension{"dalgona/mcp/config/write": call.write}
}

func (m *mcpConfigService) remember(version, cwd string) {
	m.mu.Lock()
	defer m.mu.Unlock()
	if _, known := m.issued[version]; known {
		return
	}
	if len(m.order) == mcpIssuedLimit {
		delete(m.issued, m.order[0])
		m.order = m.order[1:]
	}
	m.issued[version] = cwd
	m.order = append(m.order, version)
}

func (m *mcpConfigService) workspaceOf(version string) (string, bool) {
	m.mu.Lock()
	defer m.mu.Unlock()
	cwd, ok := m.issued[version]
	return cwd, ok
}

func (c mcpConfigCall) read(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	p, err := decodeNative[mcpReadParams](params)
	if err != nil {
		return nil, err
	}
	cwd, err := c.workspace(p.ThreadID)
	if err != nil {
		return nil, err
	}
	inventory, err := c.inventory(ctx, cwd)
	if err != nil {
		return nil, err
	}
	view := mcpConfigView{Version: inventory.Version, Servers: make([]mcpServerView, 0, len(inventory.Servers)), TrustedWorkspace: inventory.TrustedWorkspace}
	for _, server := range inventory.Servers {
		view.Servers = append(view.Servers, mcpServerView{
			Name: server.Name, Source: server.Source, Editable: server.Source == mcpSourceUser,
			Config: projectMCPConfig(server.Config), Effective: server.Effective,
		})
	}
	return encodeNative(view)
}

func (c mcpConfigCall) write(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	if len(params) > mcpWriteBytes {
		return nil, nativeFailure(rpc.ErrorLimitExceeded, "the MCP configuration edit exceeds 256 KiB; send fewer edits")
	}
	p, err := decodeNative[mcpWriteParams](params)
	if err != nil {
		return nil, err
	}
	if len(p.Edits) > mcpEditLimit {
		return nil, nativeFailure(rpc.ErrorLimitExceeded, "send at most 32 MCP configuration edits at once")
	}
	c.service.write.Lock()
	defer c.service.write.Unlock()
	cwd, ok := c.service.workspaceOf(p.ExpectedVersion)
	if !ok {
		return nil, staleMCPConfig("", "the MCP configuration version is unknown; read dalgona/mcp/config/read again")
	}
	user, err := c.expected(cwd, p.ExpectedVersion)
	if err != nil {
		return nil, err
	}
	return c.apply(ctx, cwd, user, p.Edits)
}

func (c mcpConfigCall) expected(cwd, version string) (config.UserMCPServers, error) {
	inventory, user, err := c.service.batteries.MCPConfigSnapshot(cwd)
	if err != nil {
		return config.UserMCPServers{}, nativeFailure(rpc.ErrorUnavailable, "read the MCP configuration: "+err.Error())
	}
	if inventory.Version != version {
		return config.UserMCPServers{}, staleMCPConfig(inventory.Version, "the MCP configuration changed; read dalgona/mcp/config/read again and reapply the edits")
	}
	return user, nil
}

func (c mcpConfigCall) apply(ctx context.Context, cwd string, user config.UserMCPServers, edits []mcpEdit) (jsontext.Value, error) {
	home := c.host.Config().Home
	settings, err := settingEdits(edits, user)
	if err != nil {
		return nil, err
	}
	if _, writeErr := config.WriteSettingsIf(home, user.SettingsVersion, settings...); writeErr != nil {
		if errors.Is(writeErr, config.ErrSettingsChanged) {
			current, _ := c.inventory(ctx, cwd)
			return nil, staleMCPConfig(current.Version, "settings.toml changed during the write; read dalgona/mcp/config/read again")
		}
		return nil, invalidNative(writeErr.Error())
	}
	restart, err := c.service.batteries.ReloadUserMCP()
	if err != nil {
		restart = restartRequired(c.host.MCPStatus(ctx, cwd), edits)
	}
	after, err := c.inventory(ctx, cwd)
	if err != nil {
		return nil, err
	}
	return encodeNative(struct {
		Version                string   `json:"version"`
		RestartRequiredServers []string `json:"restartRequiredServers"`
	}{after.Version, restart})
}

func (c mcpConfigCall) inventory(ctx context.Context, cwd string) (agent.MCPInventory, error) {
	inventory, err := c.host.MCPInventory(ctx, cwd)
	if err != nil {
		return agent.MCPInventory{}, nativeFailure(rpc.ErrorUnavailable, "read the MCP configuration: "+err.Error())
	}
	c.service.remember(inventory.Version, cwd)
	return inventory, nil
}

func (c mcpConfigCall) workspace(threadID string) (string, error) {
	if threadID == "" {
		return c.host.Config().Cwd, nil
	}
	for _, info := range c.host.AllThreads() {
		if info.ID == threadID {
			return info.Cwd, nil
		}
	}
	return "", invalidNative("unknown thread " + strconv.Quote(threadID))
}

func projectMCPConfig(spec config.MCPServerSpec) any {
	if spec.URL != "" {
		return mcpHTTPConfig{
			Kind: mcpKindHTTP, URL: spec.URL, BearerTokenEnv: spec.BearerTokenEnv, OAuth: spec.OAuth,
			OAuthScopes: spec.OAuthScopes, OAuthClientID: spec.OAuthClientID, OAuthCallbackPort: spec.OAuthCallbackPort,
		}
	}
	env := make(map[string]mcpEnvValue, len(spec.Env))
	for key := range spec.Env {
		env[key] = mcpEnvValue{Redacted: true}
	}
	return mcpStdioConfig{Kind: mcpKindStdio, Command: spec.Command, Args: spec.Args, Env: env}
}

func settingEdits(edits []mcpEdit, user config.UserMCPServers) ([]config.SettingEdit, error) {
	if len(edits) == 0 {
		return nil, invalidNative("edits must hold at least one edit")
	}
	seen := map[string]bool{}
	settings := make([]config.SettingEdit, 0, len(edits))
	for _, edit := range edits {
		if seen[edit.Name] {
			return nil, invalidNative("edits name server " + strconv.Quote(edit.Name) + " twice; send one edit for each server")
		}
		seen[edit.Name] = true
		setting, err := settingEdit(edit, user)
		if err != nil {
			return nil, err
		}
		settings = append(settings, setting)
	}
	return settings, nil
}

func settingEdit(edit mcpEdit, user config.UserMCPServers) (config.SettingEdit, error) {
	if err := checkServerName(edit.Name); err != nil {
		return config.SettingEdit{}, err
	}
	key := mcpServersSettings + edit.Name
	switch edit.Action {
	case mcpActionRemove:
		return config.SettingEdit{Key: key}, removable(edit, user)
	case mcpActionPut:
		if edit.Config == nil {
			return config.SettingEdit{}, invalidNative("put for " + strconv.Quote(edit.Name) + " needs a config")
		}
		table, err := edit.Config.table(edit.Name, user.Servers[edit.Name].Env)
		return config.SettingEdit{Key: key, Value: table}, err
	}
	return config.SettingEdit{}, invalidNative("action must be put or remove")
}

func removable(edit mcpEdit, user config.UserMCPServers) error {
	name := strconv.Quote(edit.Name)
	_, defined := user.Servers[edit.Name]
	switch {
	case edit.Config != nil:
		return invalidNative("remove for " + name + " takes no config")
	case user.InSettings[edit.Name]:
		return nil
	case defined:
		return invalidNative("server " + name + " is defined in config.toml; remove it there")
	}
	return invalidNative("no user server is named " + name)
}

func checkServerName(name string) error {
	policy := textinput.Policy{Lines: textinput.SingleLine, Limit: textinput.Limit{Unit: textinput.Bytes, Max: mcpNameBytes}}
	if err := textinput.CheckRequest([]textinput.Item{{Field: "edits.name", Policy: policy, Text: name}}); err != nil {
		return invalidNative(err.Error())
	}
	if name == "" || strings.ContainsAny(name, ".\"") {
		return invalidNative("server names must be nonempty and contain no dot or quotation mark")
	}
	return nil
}

func (e mcpEditConfig) table(name string, stored map[string]string) (map[string]any, error) {
	switch e.Kind {
	case mcpKindStdio:
		return e.stdioTable(name, stored)
	case mcpKindHTTP:
		return e.httpTable(name)
	}
	return nil, invalidNative("config.kind for " + strconv.Quote(name) + " must be stdio or http")
}

func (e mcpEditConfig) stdioTable(name string, stored map[string]string) (map[string]any, error) {
	if e.hasHTTPFields() {
		return nil, invalidNative("the stdio config for " + strconv.Quote(name) + " takes no HTTP fields")
	}
	if e.Command == "" {
		return nil, invalidNative("the stdio config for " + strconv.Quote(name) + " needs a command")
	}
	table := map[string]any{"command": e.Command}
	if len(e.Args) > 0 {
		table["args"] = e.Args
	}
	env, err := resolveEnv(name, e.Env, stored)
	if len(env) > 0 {
		table["env"] = env
	}
	return table, err
}

func (e mcpEditConfig) hasHTTPFields() bool {
	return e.URL != "" || e.BearerTokenEnv != "" || e.OAuth || len(e.OAuthScopes) > 0 || e.OAuthClientID != "" || e.OAuthCallbackPort != 0
}

func (e mcpEditConfig) httpTable(name string) (map[string]any, error) {
	if e.Command != "" || len(e.Args) > 0 || len(e.Env) > 0 {
		return nil, invalidNative("the http config for " + strconv.Quote(name) + " takes no command, args, or env")
	}
	if e.URL == "" {
		return nil, invalidNative("the http config for " + strconv.Quote(name) + " needs a url")
	}
	table := map[string]any{"url": e.URL}
	for key, value := range httpOptionsTable(e) {
		table[key] = value
	}
	return table, nil
}

func httpOptionsTable(e mcpEditConfig) map[string]any {
	table := map[string]any{}
	for key, value := range httpOptionalFields(e) {
		if value != "" {
			table[key] = value
		}
	}
	if e.OAuth {
		table["oauth"] = true
	}
	if len(e.OAuthScopes) > 0 {
		table["oauth_scopes"] = e.OAuthScopes
	}
	if e.OAuthCallbackPort != 0 {
		table["oauth_callback_port"] = int64(e.OAuthCallbackPort)
	}
	return table
}

func httpOptionalFields(e mcpEditConfig) map[string]string {
	return map[string]string{"bearer_token_env": e.BearerTokenEnv, "oauth_client_id": e.OAuthClientID}
}

func resolveEnv(name string, entries map[string]mcpEnvValue, stored map[string]string) (map[string]string, error) {
	env := make(map[string]string, len(entries))
	for key, entry := range entries {
		value, err := entry.resolve(name, key, stored)
		if err != nil {
			return nil, err
		}
		env[key] = value
	}
	return env, nil
}

func (e mcpEnvValue) resolve(name, key string, stored map[string]string) (string, error) {
	where := "env " + strconv.Quote(key) + " of " + strconv.Quote(name)
	switch {
	case e.Redacted:
		return "", invalidNative(where + " sends the redaction placeholder; send {value} or {preserve:true}")
	case e.Value != nil && e.Preserve:
		return "", invalidNative(where + " sets both value and preserve; send one")
	case e.Value != nil:
		return *e.Value, nil
	case !e.Preserve:
		return "", invalidNative(where + " needs {value} or {preserve:true}")
	}
	value, ok := stored[key]
	if !ok {
		return "", invalidNative(where + " has no stored value to preserve; send {value}")
	}
	return value, nil
}

func restartRequired(running []agent.MCPServerStatus, edits []mcpEdit) []string {
	names := []string{}
	for _, edit := range edits {
		if slices.ContainsFunc(running, func(status agent.MCPServerStatus) bool { return status.Name == edit.Name }) {
			names = append(names, edit.Name)
		}
	}
	slices.Sort(names)
	return names
}

func staleMCPConfig(current, message string) error {
	native := rpc.NativeError(rpc.ErrorData{Kind: rpc.ErrorStaleRevision, CurrentRevision: current})
	return &rpc.RPCError{Code: native.Code, Message: message, Data: native.Data}
}
