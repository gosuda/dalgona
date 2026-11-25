package main

import (
	"context"
	"fmt"
	"log/slog"
	"net"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/codexapp"
	"github.com/gosuda/dalgona/internal/dalgona/sandboxsvc/registry"
	"github.com/gosuda/dalgona/internal/dalgona/webui"
)

func loadConfig() (config.Config, error) {
	cfg, err := config.Load(config.ProductDalgona, "", config.Flags{})
	if err != nil {
		return config.Config{}, err
	}
	if err := config.InstallTLSRoots(cfg.TLSCAFile); err != nil {
		return config.Config{}, err
	}
	return cfg, nil
}

func openSession(cfg config.Config, std stdio) (*client.Session, error) {
	if name := std.remoteName(cfg.Remote); name != "" {
		store, err := authStore(cfg)
		if err != nil {
			return nil, err
		}
		services := registry.New(registry.Options{Auth: store, Logger: slog.New(slog.NewTextHandler(std.errOut, nil))})
		return client.Remote(client.NewRemotes(cfg, services), name), nil
	}
	session, _, err := localSession(cfg.Home)
	return session, err
}

func sessionDialer(session *client.Session) func(context.Context) (net.Conn, error) {
	return func(ctx context.Context) (net.Conn, error) { return connect(ctx, session) }
}

func runAppServer(ctx context.Context, args []string, std stdio) int {
	cfg, err := loadConfig()
	if err != nil {
		return fail(std, 1, err)
	}
	session, err := openSession(cfg, std)
	if err != nil {
		return fail(std, 1, err)
	}
	defer func() { _ = session.Close() }()
	return codexapp.AppServerMain(ctx, args, std, std.errOut, codexapp.Deps{Home: cfg.Home, Dial: sessionDialer(session)})
}

func runWeb(ctx context.Context, args []string, std stdio) int {
	if len(args) != 0 {
		return usage(std, "web")
	}
	cfg, err := loadConfig()
	if err != nil {
		return fail(std, 1, err)
	}
	session, err := openSession(cfg, std)
	if err != nil {
		return fail(std, 1, err)
	}
	defer func() { _ = session.Close() }()
	web := webui.Config{Listen: cfg.Web.Listen, Token: cfg.Web.Token, CertFile: cfg.Web.CertFile, KeyFile: cfg.Web.KeyFile}
	if cfg.Web.CertFile != "" && cfg.Web.KeyFile != "" {
		local, _, err := localSession(cfg.Home)
		if err != nil {
			return fail(std, 1, err)
		}
		defer func() { _ = local.Close() }()
		web.PairingDial = local.Connect
	}
	if err := webui.Serve(ctx, web, sessionDialer(session), std.errOut); err != nil {
		return fail(std, 1, err)
	}
	return 0
}

func runCodex(ctx context.Context, args []string, std stdio) int {
	if len(args) > 0 && args[0] == "record" {
		return codexapp.RecordMain(ctx, args[1:], std, std.errOut, config.ProductDalgona.Home())
	}
	if len(args) != 1 {
		return usage(std, "codex record|install|uninstall|status")
	}
	exe, err := executable()
	if err != nil {
		return fail(std, 1, err)
	}
	env := codexapp.Env{DalgonaHome: config.ProductDalgona.Home()}
	switch args[0] {
	case "install":
		return codexResult(std, codexapp.Install(env, exe), "Restart the Codex app to use dalgona.")
	case "uninstall":
		return codexResult(std, codexapp.Uninstall(env), "Restart the Codex app to use its bundled runtime again.")
	case "status":
		printCodexStatus(std, codexapp.Status(env, exe))
		return 0
	default:
		return usage(std, "codex record|install|uninstall|status")
	}
}

func codexResult(std stdio, err error, notice string) int {
	if err != nil {
		return fail(std, 1, err)
	}
	fmt.Fprintln(std.errOut, notice)
	return 0
}

func printCodexStatus(std stdio, mechanisms []codexapp.Mechanism) {
	for _, mechanism := range mechanisms {
		state := "not installed"
		switch {
		case mechanism.Installed && mechanism.Matches:
			state = "points at this dalgona"
		case mechanism.Installed:
			state = "points elsewhere"
		}
		fmt.Fprintf(std.out, "%s\t%s\t%s\t%s\n", mechanism.Name, mechanism.Path, mechanism.Current, state)
	}
}
