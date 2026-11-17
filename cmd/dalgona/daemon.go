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
	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/runner"
	"github.com/gosuda/dalgona/internal/dalgona/battery"
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

func ensure(ctx context.Context) (net.Conn, error) {
	exe, err := executable()
	if err != nil {
		return nil, err
	}
	return ensureHome(ctx, config.ProductDalgona.Home(), exe)
}

func ensureHome(ctx context.Context, home, exe string) (net.Conn, error) {
	for {
		conn, err := daemon.Ensure(ctx, home, exe)
		if err != nil {
			return nil, err
		}
		restarted, checkErr := ensureBuild(ctx, home)
		if checkErr != nil {
			_ = conn.Close()
			return nil, checkErr
		}
		if !restarted {
			return conn, nil
		}
	}
}

func ensureBuild(ctx context.Context, home string) (bool, error) {
	if stdinIsTerminal() {
		return confirmBuildRestart(ctx, home, daemon.Build(), os.Stdin, os.Stdout)
	}
	return false, checkBuild(ctx, home, daemon.Build())
}

func stdinIsTerminal() bool {
	info, err := os.Stdin.Stat()
	return err == nil && info.Mode()&os.ModeCharDevice != 0
}

func checkBuild(ctx context.Context, home, own string) error {
	if own == "" {
		return nil
	}
	_, err := daemon.CheckBuild(ctx, home, own)
	return err
}

func confirmBuildRestart(ctx context.Context, home, own string, in io.Reader, out io.Writer) (bool, error) {
	if own == "" {
		return false, nil
	}
	_, err := daemon.CheckBuild(ctx, home, own)
	mismatch, ok := errors.AsType[*daemon.BuildMismatchError](err)
	if !ok {
		return false, err
	}
	fmt.Fprintf(out, "daemon %s runs build %s; this client is build %s. Restart the daemon? Running turns stop and recover. [y/N] ",
		home, mismatch.Running, mismatch.Own)
	answer, _ := bufio.NewReader(in).ReadString('\n')
	if strings.TrimSpace(answer) != "y" {
		return false, errors.New("daemon build mismatch: restart the daemon with dalgona restart")
	}
	if err := daemon.Retire(ctx, home); err != nil {
		return false, err
	}
	if !daemon.WaitSocketGone(ctx, home) {
		return false, errors.New("daemon did not stop after retire")
	}
	return true, nil
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
		extensions[daemon.MethodIdentity] = identityExtension(startedAt)
		extensions[daemon.MethodRetire] = retireExtension(retireDaemon)
		options := rpc.ServeOptions{Extensions: extensions, BeforeArchive: batteries.Archive, Advisors: advisors}
		return rpc.ServeWith(ctx, conn, host, options)
	}
	if err := daemon.Run(ctx, cfg.Home, newHost, serve, batteries.AfterRecover); err != nil {
		return fail(std, 1, err)
	}
	return 0
}

func identityExtension(startedAt time.Time) rpc.Extension {
	return func(context.Context, jsontext.Value) (jsontext.Value, error) {
		var out bytes.Buffer
		encoder := jsontext.NewEncoder(&out)
		tokens := []jsontext.Token{
			jsontext.BeginObject,
			jsontext.String("build"), jsontext.String(daemon.Build()),
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

func retireExtension(cancel context.CancelFunc) rpc.Extension {
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
	exe, err := executable()
	if err != nil {
		return fail(std, 1, err)
	}
	own := daemon.Build()
	info, probe, err := daemon.ProbeDaemon(ctx, home)
	if err != nil {
		return fail(std, 1, err)
	}
	if err := refuseUnsafeProbe(probe, info); err != nil {
		return fail(std, 1, err)
	}
	if err := stopRunningDaemon(ctx, probe, home); err != nil {
		return fail(std, 1, err)
	}
	if _, err := daemon.Ensure(ctx, home, exe); err != nil {
		return fail(std, 1, err)
	}
	if probe == daemon.ProbeAbsent {
		fmt.Fprintf(std.out, "daemon started (build %s)\n", own)
		return 0
	}
	fmt.Fprintf(std.out, "daemon restarted (build %s)\n", own)
	return 0
}

func refuseUnsafeProbe(probe daemon.Probe, info daemon.Info) error {
	if probe == daemon.ProbePreIdentity || probe == daemon.ProbeReady && !daemon.VerifiedPID(info.PID) {
		return errors.New("cannot safely identify the old daemon; stop it manually and retry dalgona restart")
	}
	return nil
}

func stopRunningDaemon(ctx context.Context, probe daemon.Probe, home string) error {
	if probe != daemon.ProbeReady {
		return nil
	}
	if err := daemon.Retire(ctx, home); err != nil {
		return err
	}
	if !daemon.WaitSocketGone(ctx, home) {
		return errors.New("daemon did not stop after retire")
	}
	return nil
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
	}, auth.SecretEnvironmentName)
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
