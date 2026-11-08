package runner

import (
	"context"
	"crypto/subtle"
	"errors"
	"flag"
	"fmt"
	"io"
	"io/fs"
	"log/slog"
	"net"
	"net/http"
	"os"
	"os/signal"
	"path/filepath"
	"strings"
	"time"

	"github.com/coder/websocket"
)

const boxEnvironmentKey = "DALGONA_BOX_TOKEN"

type BridgeFunc func(ctx context.Context, conn io.ReadWriteCloser) error

type execServerConfig struct {
	listen string
	root   string
	data   string
}

type stdioConn struct {
	io.Reader
	io.Writer
}

func (stdioConn) Close() error { return nil }

func Serve(ctx context.Context, conn io.ReadWriteCloser, local *Local) error {
	return serveExecRPC(ctx, conn, local)
}

func ExecServerMain(args []string, stdin io.Reader, stdout io.Writer) int {
	return ExecServerMainWithBridge(args, stdin, stdout, nil)
}

func ExecServerMainWithBridge(args []string, stdin io.Reader, stdout io.Writer, bridge BridgeFunc) int {
	cfg, err := parseExecServerConfig(args)
	if err != nil {
		_, _ = fmt.Fprintln(stdout, err)
		return 2
	}
	if strings.HasPrefix(cfg.listen, "ws://") && len(os.Getenv(boxEnvironmentKey)) < 32 {
		_, _ = fmt.Fprintln(stdout, "exec-server: ws listen needs DALGONA_BOX_TOKEN (32+ characters)")
		return 1
	}
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt)
	defer stop()
	if err := runExecServer(ctx, cfg, stdin, stdout, bridge); err != nil {
		_, _ = fmt.Fprintln(stdout, err)
		return 1
	}
	return 0
}

func parseExecServerConfig(args []string) (execServerConfig, error) {
	flags := flag.NewFlagSet("exec-server", flag.ContinueOnError)
	flags.SetOutput(io.Discard)
	listen := flags.String("listen", "stdio://", "listen address")
	root := flags.String("root", "/workspace", "workspace root")
	data := flags.String("data", "", "persistent data directory")
	if err := flags.Parse(args); err != nil {
		return execServerConfig{}, fmt.Errorf("exec-server: %w", err)
	}
	if flags.NArg() != 0 {
		return execServerConfig{}, errors.New("exec-server: unexpected positional arguments")
	}
	cfg := execServerConfig{listen: *listen, root: *root, data: *data}
	if cfg.listen != "stdio://" && !strings.HasPrefix(cfg.listen, "ws://") {
		return execServerConfig{}, errors.New("exec-server: listen must be stdio:// or ws://IP:PORT")
	}
	return cfg, nil
}

func runExecServer(ctx context.Context, cfg execServerConfig, stdin io.Reader, stdout io.Writer, bridge BridgeFunc) error {
	root, err := prepareExecRoot(cfg.root, cfg.data)
	if err != nil {
		return err
	}
	if chdirErr := os.Chdir(root); chdirErr != nil {
		return fmt.Errorf("exec-server: chdir %s: %w", root, chdirErr)
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return fmt.Errorf("exec-server: home: %w", err)
	}
	local, err := NewLocal(filepath.Join(home, ".dalgona", "runs"))
	if err != nil {
		return err
	}
	if cfg.listen == "stdio://" {
		conn := stdioConn{Reader: stdin, Writer: stdout}
		return Serve(ctx, conn, local)
	}
	listener, err := net.Listen("tcp", strings.TrimPrefix(cfg.listen, "ws://"))
	if err != nil {
		return fmt.Errorf("exec-server: listen %s: %w", cfg.listen, err)
	}
	return serveExecWS(ctx, listener, local, os.Getenv(boxEnvironmentKey), bridge)
}

func prepareExecRoot(root, data string) (string, error) {
	if data == "" {
		if err := os.MkdirAll(root, 0o700); err != nil {
			return "", fmt.Errorf("exec-server: create root: %w", err)
		}
		return root, nil
	}
	workspace := filepath.Join(data, "workspace")
	home := filepath.Join(data, "home")
	if err := os.MkdirAll(workspace, 0o700); err != nil {
		return "", fmt.Errorf("exec-server: create workspace: %w", err)
	}
	if err := os.MkdirAll(home, 0o700); err != nil {
		return "", fmt.Errorf("exec-server: create home: %w", err)
	}
	if err := linkExecRoot(root, workspace); err != nil {
		return "", err
	}
	if err := os.Setenv("HOME", home); err != nil {
		return "", fmt.Errorf("exec-server: set HOME: %w", err)
	}
	return root, nil
}

func linkExecRoot(root, workspace string) error {
	if filepath.Clean(root) == filepath.Clean(workspace) {
		return nil
	}
	if err := os.MkdirAll(filepath.Dir(root), 0o700); err != nil {
		return fmt.Errorf("exec-server: create root parent: %w", err)
	}
	info, err := os.Lstat(root)
	if errors.Is(err, fs.ErrNotExist) {
		return createExecRootLink(root, workspace)
	}
	if err != nil {
		return fmt.Errorf("exec-server: inspect root: %w", err)
	}
	if info.Mode()&os.ModeSymlink == 0 {
		return fmt.Errorf("exec-server: root %s already exists", root)
	}
	current, err := filepath.EvalSymlinks(root)
	if err == nil && filepath.Clean(current) == filepath.Clean(workspace) {
		return nil
	}
	if err := os.Remove(root); err != nil {
		return fmt.Errorf("exec-server: replace root link: %w", err)
	}
	return createExecRootLink(root, workspace)
}

func createExecRootLink(root, workspace string) error {
	if err := os.Symlink(workspace, root); err != nil {
		return fmt.Errorf("exec-server: link root: %w", err)
	}
	return nil
}

func serveExecWS(ctx context.Context, listener net.Listener, local *Local, token string, bridge BridgeFunc) error {
	server := &http.Server{Handler: execWSHandler(ctx, local, token, bridge), ReadHeaderTimeout: 10 * time.Second}
	errCh := make(chan error, 1)
	go func() { errCh <- server.Serve(listener) }()
	return waitExecListener(ctx, server, errCh)
}

func execWSHandler(ctx context.Context, local *Local, token string, bridge BridgeFunc) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if !bearerAuthorized(r.Header.Get("Authorization"), token) {
			w.Header().Set("WWW-Authenticate", "Bearer")
			http.Error(w, "unauthorized", http.StatusUnauthorized)
			return
		}
		routeExecWS(ctx, local, bridge, w, r)
	})
}

func routeExecWS(ctx context.Context, local *Local, bridge BridgeFunc, w http.ResponseWriter, r *http.Request) {
	switch r.URL.Path {
	case "/":
		serveExecWebsocket(ctx, w, r, local, nil)
	case "/bridge":
		if bridge == nil {
			http.Error(w, "bridge unavailable", http.StatusServiceUnavailable)
			return
		}
		serveExecWebsocket(ctx, w, r, nil, bridge)
	default:
		http.NotFound(w, r)
	}
}

func waitExecListener(ctx context.Context, server *http.Server, errCh <-chan error) error {
	select {
	case err := <-errCh:
		if errors.Is(err, http.ErrServerClosed) {
			return nil
		}
		return fmt.Errorf("exec-server: websocket serve: %w", err)
	case <-ctx.Done():
		return shutdownExecListener(server, errCh)
	}
}

func shutdownExecListener(server *http.Server, errCh <-chan error) error {
	shutdownCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err := server.Shutdown(shutdownCtx); err != nil {
		return fmt.Errorf("exec-server: websocket shutdown: %w", err)
	}
	err := <-errCh
	if err != nil && !errors.Is(err, http.ErrServerClosed) {
		return fmt.Errorf("exec-server: websocket serve: %w", err)
	}
	return nil
}

func bearerAuthorized(header, token string) bool {
	provided, ok := strings.CutPrefix(header, "Bearer ")
	return ok && subtle.ConstantTimeCompare([]byte(provided), []byte(token)) == 1
}

func serveExecWebsocket(ctx context.Context, w http.ResponseWriter, r *http.Request, local *Local, bridge BridgeFunc) {
	ws, err := websocket.Accept(w, r, nil)
	if err != nil {
		slog.Debug("exec-server: websocket accept", "error", err)
		return
	}
	conn := websocket.NetConn(ctx, ws, websocket.MessageText)
	defer func() { _ = conn.Close() }()
	if bridge != nil {
		err = bridge(ctx, conn)
	} else {
		err = Serve(ctx, conn, local)
	}
	if err != nil && ctx.Err() == nil {
		slog.Debug("exec-server: websocket connection", "error", err)
	}
}
