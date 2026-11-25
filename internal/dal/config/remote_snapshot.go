package config

import "path/filepath"

func LoadRemoteSnapshot(home, name string) (RemoteSpec, string, error) {
	cfg := Config{Home: home}
	var spec RemoteSpec
	base := readLayer(filepath.Join(home, "config.toml"))
	if base.err != nil {
		return spec, "", base.err
	}
	if base.doc != nil {
		spec = cfg.mergeRemote(spec, base.doc.Remotes[name])
	}
	settings := readLayer(filepath.Join(home, "settings.toml"))
	if settings.err != nil {
		return spec, "", settings.err
	}
	if settings.doc != nil {
		spec = cfg.mergeRemote(spec, settings.doc.Remotes[name])
	}
	return spec, digestSettings(settings.raw), nil
}
