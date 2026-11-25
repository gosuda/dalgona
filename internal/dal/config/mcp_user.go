package config

import "path/filepath"

type UserMCPServers struct {
	Servers         map[string]MCPServerSpec
	InSettings      map[string]bool
	SettingsVersion string
}

func LoadUserMCPServers(home string) (UserMCPServers, error) {
	merged := defaultConfigWithHome(ProductDalgona, home, "")
	if _, err := merged.mergeMCPFile(filepath.Join(home, "config.toml")); err != nil {
		return UserMCPServers{}, err
	}
	settings, err := merged.mergeMCPFile(filepath.Join(home, "settings.toml"))
	if err != nil {
		return UserMCPServers{}, err
	}
	user := UserMCPServers{Servers: merged.MCPServers, InSettings: map[string]bool{}, SettingsVersion: digestSettings(settings.raw)}
	if settings.doc != nil {
		for name := range settings.doc.MCPServers {
			user.InSettings[name] = true
		}
	}
	return user, nil
}

func (c *Config) mergeMCPFile(path string) (layer, error) {
	current := readLayer(path)
	if current.err != nil || current.doc == nil {
		return current, current.err
	}
	return current, c.mergeMCPServers(current)
}
