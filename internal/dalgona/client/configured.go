package client

import (
	"log/slog"

	"github.com/gosuda/dalgona/internal/dal/auth"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dalgona/sandboxsvc/registry"
)

func OpenConfigured(backend Backend, observer Observer, logger *slog.Logger) (*EndpointSet, *Remotes, error) {
	if err := config.AnchorHome(); err != nil {
		return nil, nil, err
	}
	cfg, err := config.Load(config.ProductDalgona, "", config.Flags{})
	if err != nil {
		return nil, nil, err
	}
	if err = config.InstallTLSRoots(cfg.TLSCAFile); err != nil {
		return nil, nil, err
	}
	store, err := auth.NewWithPatterns(cfg.Home, cfg.Secrets.Patterns, auth.WithBuiltinPatterns(cfg.Secrets.Builtin))
	if err != nil {
		return nil, nil, err
	}
	remotes := NewRemotes(cfg, registry.New(registry.Options{Auth: store, Logger: logger}))
	set, err := NewEndpointSet(remotes, ConfiguredEndpoints(cfg, backend, remotes), observer, ProtocolNative)
	if err != nil {
		return nil, nil, err
	}
	return set, remotes, nil
}
