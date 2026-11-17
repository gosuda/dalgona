package config

import (
	"errors"
	"fmt"
	"maps"
	"path/filepath"
	"regexp"
	"slices"

	"github.com/gosuda/dalgona/internal/dal/auth/oauth"
)

const (
	defaultMCPCallbackPort = 54871
	minMCPCallbackPort     = 1024
	maxMCPCallbackPort     = 65535
)

var (
	mcpOAuthNamePattern = regexp.MustCompile(`^[a-z0-9_-]{1,32}$`)
	envVarNamePattern   = regexp.MustCompile(`^[A-Za-z_][A-Za-z0-9_]*$`)
)

type mcpLocator struct {
	file  string
	name  string
	lines map[string]origin
}

func (l mcpLocator) has(key string) bool {
	_, ok := l.lines["mcp_servers."+l.name+"."+key]
	return ok
}

func (l mcpLocator) fail(key, message string) error {
	for _, candidate := range []string{"mcp_servers." + l.name + "." + key, "mcp_servers." + l.name, "mcp_servers"} {
		if location, ok := l.lines[candidate]; ok {
			return fmt.Errorf("%s:%d: [mcp_servers.%s] %s", location.file, location.line, l.name, message)
		}
	}
	return fmt.Errorf("%s:1: [mcp_servers.%s] %s", l.file, l.name, message)
}

func (c *Config) mergeMCPServers(current layer) error {
	lines := lineMap(current.path, current.raw)
	var errs []error
	for _, name := range slices.Sorted(maps.Keys(current.doc.MCPServers)) {
		doc := current.doc.MCPServers[name]
		at := mcpLocator{file: filepath.Base(current.path), name: name, lines: lines}
		errs = append(errs, validateMCPAuthKeys(at, doc)...)
		errs = append(errs, validateMCPOAuth(at, doc)...)
		spec := MCPServerSpec(doc)
		if !at.has("oauth_callback_port") {
			spec.OAuthCallbackPort = defaultMCPCallbackPort
		}
		c.MCPServers[name] = spec
	}
	return errors.Join(errs...)
}

func validateMCPAuthKeys(at mcpLocator, doc mcpDoc) []error {
	var errs []error
	bearer := at.has("bearer_token_env")
	if bearer && doc.OAuth {
		errs = append(errs, at.fail("bearer_token_env", "bearer_token_env and oauth exclude each other"))
	}
	if (bearer || doc.OAuth) && doc.URL == "" {
		errs = append(errs, at.fail("url", "bearer_token_env and oauth need url"))
	}
	if bearer && !envVarNamePattern.MatchString(doc.BearerTokenEnv) {
		errs = append(errs, at.fail("bearer_token_env", "bearer_token_env must name an environment variable"))
	}
	return errs
}

func validateMCPOAuth(at mcpLocator, doc mcpDoc) []error {
	var errs []error
	if doc.OAuth && !mcpOAuthNamePattern.MatchString(at.name) {
		errs = append(errs, at.fail("oauth", "oauth needs a name of 1 to 32 characters from a-z, 0-9, _ and -"))
	}
	if doc.OAuth && doc.URL != "" && !oauth.SecureURL(doc.URL) {
		errs = append(errs, at.fail("url", "oauth needs an https url"))
	}
	if at.has("oauth_callback_port") && (doc.OAuthCallbackPort < minMCPCallbackPort || doc.OAuthCallbackPort > maxMCPCallbackPort) {
		errs = append(errs, at.fail("oauth_callback_port", "oauth_callback_port must be between 1024 and 65535"))
	}
	return errs
}
