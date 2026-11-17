package config_test

import (
	"slices"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/config"
)

func TestLoad_mcpOAuthKeys(t *testing.T) {
	body := `[mcp_servers.hosted]
url = "https://mcp.example.test/mcp"
oauth = true
oauth_scopes = ["read", "write"]
oauth_client_id = "client-1"
oauth_callback_port = 8765

[mcp_servers.token]
url = "https://token.example.test/mcp"
bearer_token_env = "HOSTED_TOKEN"

[mcp_servers.plain]
url = "https://plain.example.test/mcp"
`
	cfg, err := loadConfig(t, config.ProductDalgona, body, "", config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	hosted := cfg.MCPServers["hosted"]
	requireConfigValue(t, hosted.OAuth, true)
	requireConfigValue(t, hosted.OAuthClientID, "client-1")
	requireConfigValue(t, hosted.OAuthCallbackPort, 8765)
	requireConfigValue(t, slices.Equal(hosted.OAuthScopes, []string{"read", "write"}), true)
	token := cfg.MCPServers["token"]
	requireConfigValue(t, token.BearerTokenEnv, "HOSTED_TOKEN")
	requireConfigValue(t, token.OAuth, false)
	requireConfigValue(t, token.OAuthCallbackPort, 54871)
	requireConfigValue(t, cfg.MCPServers["plain"].BearerTokenEnv, "")
}

func TestLoad_mcpOAuthErrors(t *testing.T) {
	tests := []struct {
		name string
		body string
		want string
	}{
		{
			name: "exclusive",
			body: "[mcp_servers.a]\nurl = \"https://x.test/mcp\"\nbearer_token_env = \"TOKEN\"\noauth = true\n",
			want: "config.toml:3: [mcp_servers.a] bearer_token_env and oauth exclude each other",
		},
		{
			name: "bearer needs url",
			body: "[mcp_servers.a]\nbearer_token_env = \"TOKEN\"\n",
			want: "config.toml:1: [mcp_servers.a] bearer_token_env and oauth need url",
		},
		{
			name: "oauth needs url",
			body: "[mcp_servers.a]\noauth = true\n",
			want: "config.toml:1: [mcp_servers.a] bearer_token_env and oauth need url",
		},
		{
			name: "bearer must name a variable",
			body: "[mcp_servers.a]\nurl = \"https://x.test/mcp\"\nbearer_token_env = \"NOT-A-NAME\"\n",
			want: "config.toml:3: [mcp_servers.a] bearer_token_env must name an environment variable",
		},
		{
			name: "empty bearer name",
			body: "[mcp_servers.a]\nurl = \"https://x.test/mcp\"\nbearer_token_env = \"\"\n",
			want: "config.toml:3: [mcp_servers.a] bearer_token_env must name an environment variable",
		},
		{
			name: "uppercase server name",
			body: "[mcp_servers.Bad_Name]\nurl = \"https://x.test/mcp\"\noauth = true\n",
			want: "config.toml:3: [mcp_servers.Bad_Name] oauth needs a name of 1 to 32 characters from a-z, 0-9, _ and -",
		},
		{
			name: "long server name",
			body: "[mcp_servers." + strings.Repeat("a", 33) + "]\nurl = \"https://x.test/mcp\"\noauth = true\n",
			want: "config.toml:3: [mcp_servers." + strings.Repeat("a", 33) + "] oauth needs a name of 1 to 32 characters from a-z, 0-9, _ and -",
		},
		{
			name: "cleartext url",
			body: "[mcp_servers.a]\nurl = \"http://example.test/mcp\"\noauth = true\n",
			want: "config.toml:2: [mcp_servers.a] oauth needs an https url",
		},
		{
			name: "port below range",
			body: "[mcp_servers.a]\nurl = \"https://x.test/mcp\"\noauth = true\noauth_callback_port = 1023\n",
			want: "config.toml:4: [mcp_servers.a] oauth_callback_port must be between 1024 and 65535",
		},
		{
			name: "port above range",
			body: "[mcp_servers.a]\nurl = \"https://x.test/mcp\"\noauth = true\noauth_callback_port = 65536\n",
			want: "config.toml:4: [mcp_servers.a] oauth_callback_port must be between 1024 and 65535",
		},
		{
			name: "explicit zero port",
			body: "[mcp_servers.a]\nurl = \"https://x.test/mcp\"\noauth = true\noauth_callback_port = 0\n",
			want: "config.toml:4: [mcp_servers.a] oauth_callback_port must be between 1024 and 65535",
		},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			_, err := loadConfig(t, config.ProductDalgona, test.body, "", config.Flags{})
			requireConfigError(t, err, test.want)
		})
	}
}

func TestLoad_mcpOAuthAcceptedBoundaries(t *testing.T) {
	tests := []struct {
		name string
		body string
		port int
	}{
		{"lowest port", "[mcp_servers.a]\nurl = \"https://x.test/mcp\"\noauth = true\noauth_callback_port = 1024\n", 1024},
		{"highest port", "[mcp_servers.a]\nurl = \"https://x.test/mcp\"\noauth = true\noauth_callback_port = 65535\n", 65535},
		{"loopback address", "[mcp_servers.a]\nurl = \"http://127.0.0.1:9/mcp\"\noauth = true\n", 54871},
		{"localhost", "[mcp_servers.a]\nurl = \"http://localhost:9/mcp\"\noauth = true\n", 54871},
		{"longest name", "[mcp_servers." + strings.Repeat("a", 32) + "]\nurl = \"https://x.test/mcp\"\noauth = true\n", 54871},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			cfg, err := loadConfig(t, config.ProductDalgona, test.body, "", config.Flags{})
			if err != nil {
				t.Fatal(err)
			}
			for _, server := range cfg.MCPServers {
				requireConfigValue(t, server.OAuthCallbackPort, test.port)
			}
		})
	}
}

func TestLoad_mcpServerReplacedByLaterLayerKeepsNoStalePort(t *testing.T) {
	configBody := "[mcp_servers.a]\nurl = \"https://x.test/mcp\"\noauth = true\noauth_callback_port = 5000\n"
	settingsBody := "[mcp_servers.a]\nurl = \"https://y.test/mcp\"\noauth = true\n"
	cfg, err := loadConfig(t, config.ProductDalgona, configBody, settingsBody, config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	requireConfigValue(t, cfg.MCPServers["a"].URL, "https://y.test/mcp")
	requireConfigValue(t, cfg.MCPServers["a"].OAuthCallbackPort, 54871)
	_, err = loadConfig(t, config.ProductDalgona, configBody, "[mcp_servers.a]\nurl = \"http://y.test/mcp\"\noauth = true\n", config.Flags{})
	requireConfigError(t, err, "settings.toml:2: [mcp_servers.a] oauth needs an https url")
}
