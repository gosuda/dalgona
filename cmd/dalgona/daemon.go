package main

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"maps"
	"net"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/agent/advisor"
	"github.com/gosuda/dalgona/internal/dal/app"
	"github.com/gosuda/dalgona/internal/dal/auth"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/rpcserver"
	"github.com/gosuda/dalgona/internal/dal/runner"
	"github.com/gosuda/dalgona/internal/dalgona/battery"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/daemon"
	"github.com/gosuda/dalgona/internal/dalgona/discovery"
	"github.com/gosuda/dalgona/internal/dalgona/report"
)

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

func localSession(home string) (*client.Session, client.Backend, error) {
	backend, err := client.CurrentBackend()
	if err != nil {
		return nil, backend, err
	}
	return client.Local(home, backend), backend, nil
}

func connect(ctx context.Context, session *client.Session) (net.Conn, error) {
	return connectPrompting(ctx, session, stdinIsTerminal(), os.Stdin, os.Stdout)
}

func connectPrompting(ctx context.Context, session *client.Session, interactive bool, in io.Reader, out io.Writer) (net.Conn, error) {
	for {
		conn, err := session.Connect(ctx)
		mismatch, ok := errors.AsType[*client.BuildMismatchError](err)
		if !ok || !interactive {
			return conn, err
		}
		if err := confirmBuildRestart(ctx, session, mismatch, in, out); err != nil {
			return nil, err
		}
	}
}

func stdinIsTerminal() bool {
	info, err := os.Stdin.Stat()
	return err == nil && info.Mode()&os.ModeCharDevice != 0
}

func confirmBuildRestart(ctx context.Context, session *client.Session, mismatch *client.BuildMismatchError, in io.Reader, out io.Writer) error {
	fmt.Fprintf(out, "daemon %s runs build %s; this client is build %s. Restart the daemon? Running turns stop and recover. [y/N] ",
		session.Endpoint().Home, mismatch.Running, mismatch.Own)
	answer, _ := bufio.NewReader(in).ReadString('\n')
	if strings.TrimSpace(answer) != "y" {
		return errors.New("daemon build mismatch: restart the daemon with dalgona restart")
	}
	_, err := session.Restart(ctx, mismatch.Daemon)
	return err
}

func runDaemon(ctx context.Context, args []string, std stdio) int {
	if len(args) != 0 {
		return usage(std, "daemon")
	}
	build := client.Build()
	cfg, err := loadConfig()
	if err != nil {
		return fail(std, 1, err)
	}
	logger := slog.New(slog.NewTextHandler(std.errOut, nil))
	batteries := &battery.Daemon{}
	batteries.WorkspaceApprover = workspaceTrustDecision
	batteries.ReporterOpener = func(ctx context.Context, home string) (*report.Reporter, error) {
		return report.OpenConsented(ctx, report.OpenParams{
			Home: cfg.Home, Enabled: cfg.FailureReports.Enabled, Method: cfg.FailureReports.Auth,
			Version: config.Version(), HomeOverridden: os.Getenv("DALGONA_HOME") != "", Notice: std.errOut,
		})
	}
	advisors := advisor.NewRegistry()
	newHost := func(ctx context.Context) (*agent.Host, func() error, error) {
		return app.NewHost(ctx, app.Options{
			Product: config.ProductDalgona, Config: &cfg, Logger: logger, Batteries: batteries.Load, Advisors: advisors,
		})
	}
	remotes := remoteExtensions(batteries)
	ctx, retireDaemon := context.WithCancel(ctx)
	defer retireDaemon()
	startedAt := time.Now()
	serve := func(ctx context.Context, conn io.ReadWriteCloser, host *agent.Host) error {
		extensions := maps.Clone(remotes)
		extensions[methodScriptRun] = scriptExtension(host, batteries)
		extensions[client.MethodIdentity] = identityExtension(build, startedAt)
		extensions[client.MethodRetire] = retireExtension(retireDaemon)
		options := rpcserver.ServeOptions{Extensions: extensions, BeforeArchive: batteries.Archive, Advisors: advisors}
		return rpcserver.ServeWith(ctx, conn, host, options)
	}
	if err := daemon.Run(ctx, cfg.Home, newHost, serve, batteries.AfterRecover); err != nil {
		return fail(std, 1, err)
	}
	return 0
}

func identityExtension(build string, startedAt time.Time) rpcserver.Extension {
	return func(context.Context, jsontext.Value) (jsontext.Value, error) {
		var out bytes.Buffer
		encoder := jsontext.NewEncoder(&out)
		tokens := []jsontext.Token{
			jsontext.BeginObject,
			jsontext.String("build"), jsontext.String(build),
			jsontext.String("pid"), jsontext.Int(int64(os.Getpid())),
			jsontext.String("version"), jsontext.String(config.Version()),
			jsontext.String("startedAt"), jsontext.String(startedAt.Format(time.RFC3339Nano)),
			jsontext.EndObject,
		}
		for _, token := range tokens {
			if err := encoder.WriteToken(token); err != nil {
				return nil, fmt.Errorf("encode daemon identity: %w", err)
			}
		}
		return jsontext.Value(bytes.TrimSpace(out.Bytes())), nil
	}
}

func retireExtension(cancel context.CancelFunc) rpcserver.Extension {
	return func(context.Context, jsontext.Value) (jsontext.Value, error) {
		cancel()
		return jsontext.Value("{}"), nil
	}
}

func runRestart(ctx context.Context, args []string, std stdio) int {
	if len(args) != 0 {
		return usage(std, "restart")
	}
	return restartDaemon(ctx, config.ProductDalgona.Home(), std)
}

func restartDaemon(ctx context.Context, home string, std stdio) int {
	session, backend, err := localSession(home)
	if err != nil {
		return fail(std, 1, err)
	}
	defer func() { _ = session.Close() }()
	running, _, err := client.ProbeDaemon(ctx, home)
	if err != nil {
		return fail(std, 1, err)
	}
	retired, err := session.Restart(ctx, running)
	if err != nil {
		return fail(std, 1, err)
	}
	if !retired {
		fmt.Fprintf(std.out, "daemon started (build %s)\n", backend.Build)
		return 0
	}
	fmt.Fprintf(std.out, "daemon restarted (build %s)\n", backend.Build)
	return 0
}

func workspaceTrustDecision(ctx context.Context, cwd string, servers []discovery.MCPServer) battery.TrustDecision {
	thread, ok := agent.ThreadFromContext(ctx)
	if !ok {
		return battery.TrustCancel
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

func bridge(ctx context.Context, peer io.ReadWriter) error {
	session, _, err := localSession(config.ProductDalgona.Home())
	if err != nil {
		return err
	}
	defer func() { _ = session.Close() }()
	conn, err := connect(ctx, session)
	if err != nil {
		return err
	}
	return splice(ctx, peer, conn)
}

func runExecServer(_ context.Context, args []string, std stdio) int {
	return runner.ExecServerMainWithBridge(args, std.in, std.out, func(ctx context.Context, conn io.ReadWriteCloser) error {
		return bridge(ctx, conn)
	}, auth.SecretEnvironmentName)
}

func splice(ctx context.Context, peer io.ReadWriter, conn net.Conn) error {
	stop := context.AfterFunc(ctx, func() { _ = conn.Close() })
	defer stop()
	go func() {
		_, copyErr := io.Copy(conn, peer)
		if copyErr != nil && !errors.Is(copyErr, net.ErrClosed) {
			slog.Debug("bridge: client to daemon", "error", copyErr)
		}
		if half, ok := conn.(interface{ CloseWrite() error }); ok {
			_ = half.CloseWrite()
		}
	}()
	_, err := io.Copy(peer, conn)
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
