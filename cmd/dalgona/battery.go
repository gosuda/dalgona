package main

import (
	"cmp"
	"context"
	"fmt"
	"net"

	"github.com/gosuda/dalgona/internal/codexapp"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/webui"
)

func loadConfig() (config.Config, error) {
	return config.Load(config.ProductDalgona, "", config.Flags{})
}

func daemonDialer(cfg config.Config, std stdio) (func(context.Context) (net.Conn, error), error) {
	if name := cmp.Or(std.remote, cfg.Remote); name != "" {
		return remoteDialer(cfg, name, std)
	}
	return ensure, nil
}

func runAppServer(ctx context.Context, args []string, std stdio) int {
	cfg, err := loadConfig()
	if err != nil {
		return fail(std, 1, err)
	}
	dial, err := daemonDialer(cfg, std)
	if err != nil {
		return fail(std, 1, err)
	}
	return codexapp.AppServerMain(ctx, args, std, std.errOut, codexapp.Deps{Home: cfg.Home, Dial: dial})
}

func runWeb(ctx context.Context, args []string, std stdio) int {
	if len(args) != 0 {
		return usage(std, "web")
	}
	cfg, err := loadConfig()
	if err != nil {
		return fail(std, 1, err)
	}
	dial, err := daemonDialer(cfg, std)
	if err != nil {
		return fail(std, 1, err)
	}
	web := webui.Config{Listen: cfg.Web.Listen, Token: cfg.Web.Token, CertFile: cfg.Web.CertFile, KeyFile: cfg.Web.KeyFile}
	if err := webui.Serve(ctx, web, dial, std.errOut); err != nil {
		return fail(std, 1, err)
	}
	return 0
}

func runCodex(ctx context.Context, args []string, std stdio) int {
	if len(args) > 0 && args[0] == "record" {
		return codexapp.RecordMain(ctx, args[1:], std, std.errOut, home())
	}
	if len(args) != 1 {
		return usage(std, "codex record|install|uninstall|status")
	}
	exe, err := executable()
	if err != nil {
		return fail(std, 1, err)
	}
	env := codexapp.Env{DalgonaHome: home()}
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
