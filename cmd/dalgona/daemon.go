package main

import (
	"context"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"maps"
	"net"
	"os"
	"path/filepath"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/agent/advisor"
	"github.com/gosuda/dalgona/internal/app"
	"github.com/gosuda/dalgona/internal/app/battery"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/daemon"
	"github.com/gosuda/dalgona/internal/discovery"
	"github.com/gosuda/dalgona/internal/rpc"
	"github.com/gosuda/dalgona/internal/runner"
)

func home() string { return config.ProductDalgona.Home() }

func executable() (string, error) {
	exe, err := os.Executable()
	if err != nil {
		return "", fmt.Errorf("locate executable: %w", err)
	}
	if resolved, resolveErr := filepath.EvalSymlinks(exe); resolveErr == nil {
		exe = resolved
	}
	return filepath.Abs(exe)
}

func ensure(ctx context.Context) (net.Conn, error) {
	exe, err := executable()
	if err != nil {
		return nil, err
	}
	return daemon.Ensure(ctx, home(), exe)
}

func runDaemon(ctx context.Context, args []string, std stdio) int {
	if len(args) != 0 {
		return usage(std, "daemon")
	}
	cfg, err := loadConfig()
	if err != nil {
		return fail(std, 1, err)
	}
	logger := slog.New(slog.NewTextHandler(std.errOut, nil))
	batteries := &battery.Daemon{}
	batteries.WorkspaceApprover = workspaceTrustDecision
	advisors := advisor.NewRegistry()
	newHost := func(ctx context.Context) (*agent.Host, func() error, error) {
		return app.NewHost(ctx, app.Options{
			Product: config.ProductDalgona, Config: &cfg, Logger: logger, Batteries: batteries.Load, Advisors: advisors,
		})
	}
	remotes := remoteExtensions(batteries)
	serve := func(ctx context.Context, conn io.ReadWriteCloser, host *agent.Host) error {
		extensions := maps.Clone(remotes)
		extensions[methodScriptRun] = scriptExtension(host, batteries)
		options := rpc.ServeOptions{Extensions: extensions, BeforeArchive: batteries.Archive, Advisors: advisors}
		return rpc.ServeWith(ctx, conn, host, options)
	}
	if err := daemon.Run(ctx, cfg.Home, newHost, serve); err != nil {
		return fail(std, 1, err)
	}
	return 0
}

func workspaceTrustDecision(ctx context.Context, cwd string, servers []discovery.MCPServer) battery.TrustDecision {
	thread, ok := agent.ThreadFromContext(ctx)
	if !ok || thread == nil {
		return battery.TrustRefuse
	}
	req := agent.WorkspaceApprovalRequest{Cwd: cwd, Servers: workspaceApprovalServers(servers)}
	reply, err := thread.RequestWorkspaceApproval(ctx, req)
	if err != nil && reply != agent.ReplyCancel {
		return battery.TrustRefuse
	}
	switch reply {
	case agent.ReplyAccept, agent.ReplyAcceptSession:
		return battery.TrustAccept
	case agent.ReplyCancel:
		return battery.TrustCancel
	case agent.ReplyDecline:
		return battery.TrustRefuse
	default:
		return battery.TrustRefuse
	}
}

func workspaceApprovalServers(servers []discovery.MCPServer) []agent.WorkspaceServer {
	out := make([]agent.WorkspaceServer, 0, len(servers))
	for _, server := range servers {
		out = append(out, agent.WorkspaceServer{
			Name: server.Name, Command: server.Command, Args: server.Args, Env: server.Env, URL: server.URL,
		})
	}
	return out
}

func runService(_ context.Context, args []string, std stdio) int {
	if len(args) != 1 || (args[0] != "install" && args[0] != "uninstall") {
		return usage(std, "service install|uninstall")
	}
	userHome, err := os.UserHomeDir()
	if err != nil {
		return fail(std, 1, fmt.Errorf("user home: %w", err))
	}
	env := daemon.ServiceEnv{Home: userHome}
	if args[0] == "uninstall" {
		err = daemon.UninstallService(env)
	} else {
		err = installService(env)
	}
	if err != nil {
		return fail(std, 1, err)
	}
	return 0
}

func installService(env daemon.ServiceEnv) error {
	exe, err := executable()
	if err != nil {
		return err
	}
	return daemon.InstallService(env, exe)
}

func runBridge(ctx context.Context, args []string, std stdio) int {
	if len(args) != 0 {
		return usage(std, "bridge")
	}
	if err := bridge(ctx, std); err != nil {
		return fail(std, 1, err)
	}
	return 0
}

func bridge(ctx context.Context, client io.ReadWriter) error {
	conn, err := ensure(ctx)
	if err != nil {
		return err
	}
	return splice(ctx, client, conn)
}

func runExecServer(_ context.Context, args []string, std stdio) int {
	return runner.ExecServerMainWithBridge(args, std.in, std.out, func(ctx context.Context, conn io.ReadWriteCloser) error {
		return bridge(ctx, conn)
	})
}

func splice(ctx context.Context, client io.ReadWriter, conn net.Conn) error {
	stop := context.AfterFunc(ctx, func() { _ = conn.Close() })
	defer stop()
	go func() {
		_, copyErr := io.Copy(conn, client)
		if copyErr != nil && !errors.Is(copyErr, net.ErrClosed) {
			slog.Debug("bridge: client to daemon", "error", copyErr)
		}
		if half, ok := conn.(interface{ CloseWrite() error }); ok {
			_ = half.CloseWrite()
		}
	}()
	_, err := io.Copy(client, conn)
	closeErr := conn.Close()
	if err != nil && !errors.Is(err, net.ErrClosed) {
		return fmt.Errorf("bridge: daemon to client: %w", err)
	}
	if closeErr != nil && !errors.Is(closeErr, net.ErrClosed) {
		return fmt.Errorf("bridge: close daemon connection: %w", closeErr)
	}
	return nil
}

func usage(std stdio, command string) int {
	fmt.Fprintf(std.errOut, "usage: %s %s\n", binary, command)
	return 2
}
