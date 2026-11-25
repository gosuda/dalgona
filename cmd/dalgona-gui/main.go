//go:build gui

package main

import (
	"context"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"log/slog"
	"net"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"sync"
	"time"

	"gioui.org/app"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/doctor"
	"github.com/gosuda/dalgona/internal/dalgona/gui/figure"
	"github.com/gosuda/dalgona/internal/dalgona/gui/manage"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

const (
	backendVariable = "DALGONA_GUI_BACKEND"
	preferenceFile  = "notifications"
	themeFile       = "theme"
)

func serveFigureChild() int {
	fonts, err := bundledFigureFonts()
	if err != nil {
		fmt.Fprintln(os.Stderr, "dalgona-gui:", err)
		return 2
	}
	return figure.ServeChild(fonts)
}

func bundledFigureFonts() (figure.Fonts, error) {
	mathData, err := platform.MathFontData()
	if err != nil {
		return figure.Fonts{}, fmt.Errorf("load the bundled math font: %w", err)
	}
	return figure.Fonts{Math: mathData, Text: platform.MathTextFaceData()}, nil
}

func newFigureSupervisor(exe string) (*figure.Supervisor, error) {
	fonts, err := bundledFigureFonts()
	if err != nil {
		return nil, err
	}
	return figure.NewSupervisor(figure.SupervisorConfig{Executable: exe, Fonts: fonts})
}

func main() {
	if figure.IsChild(os.Args) {
		os.Exit(serveFigureChild())
	}
	go func() {
		if err := run(); err != nil {
			fmt.Fprintln(os.Stderr, "dalgona-gui:", err)
			os.Exit(1)
		}
		os.Exit(0)
	}()
	app.Main()
}

func backendPath(exe string) string {
	if path := os.Getenv(backendVariable); path != "" {
		return path
	}
	dir := filepath.Dir(exe)
	if runtime.GOOS == "windows" {
		return filepath.Join(dir, "dalgona.exe")
	}
	bundled := filepath.Join(dir, "..", "lib", "dalgona-gui", "dalgona")
	if _, err := os.Stat(bundled); err == nil {
		return bundled
	}
	return filepath.Join(dir, "dalgona")
}

func stateDir() (string, error) {
	base, err := os.UserConfigDir()
	if err != nil {
		return "", fmt.Errorf("find the user configuration directory: %w", err)
	}
	dir := filepath.Join(base, "dalgona", "gui")
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return "", fmt.Errorf("create the GUI state directory %s: %w", dir, err)
	}
	return dir, nil
}

func run() error {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	logger := slog.New(slog.NewTextHandler(os.Stderr, nil))
	exe, err := os.Executable()
	if err != nil {
		return fmt.Errorf("locate the dalgona-gui executable: %w", err)
	}
	backend, err := client.ResolveBackend(backendPath(exe))
	if err != nil {
		return fmt.Errorf("resolve the dalgona backend (set %s to its path): %w", backendVariable, err)
	}
	dir, err := stateDir()
	if err != nil {
		return err
	}
	sup, err := newFigureSupervisor(exe)
	if err != nil {
		return fmt.Errorf("start the figure renderer: %w", err)
	}
	defer sup.Close()
	a := newApplication(ctx, dir, sup)
	defer func() {
		if closeErr := a.notifier.Close(); closeErr != nil {
			logger.Warn("close desktop notifications", "error", closeErr)
		}
	}()
	set, remotes, err := client.OpenConfigured(backend, a.observer, logger)
	if err != nil {
		return err
	}
	defer func() { _ = set.Close() }()
	a.attachHub(manage.NewHub(ctx, manage.Deps{
		Session: set, Lifecycle: lifecycle{set: set, remotes: remotes, backend: backend},
		Choose: a.choose, Browse: a.browse, Invalidate: a.invalidate, Now: time.Now,
	}))
	stopped := a.runOwner(ctx, set, logger)
	err = a.serve(ctx, stopped)
	cancel()
	<-stopped
	return err
}

type lifecycle struct {
	set     *client.EndpointSet
	remotes *client.Remotes
	backend client.Backend
}

func (l lifecycle) Endpoints() []client.EndpointStatus { return l.set.Endpoints() }

func (l lifecycle) Remote(name string) (client.RemoteSpec, error) { return l.remotes.Spec(name) }

func (l lifecycle) SaveRemote(name string, spec client.RemoteSpec) error {
	return l.set.SaveRemote(name, spec)
}

func (l lifecycle) Disconnect(id client.EndpointID) error { return l.set.Disconnect(id) }

func (l lifecycle) CloudUp(ctx context.Context, req client.CloudUpRequest) (client.CloudBox, error) {
	return l.remotes.CloudUp(ctx, req)
}

func (l lifecycle) CloudPull(ctx context.Context, req client.CloudPullRequest) (bool, error) {
	return l.remotes.CloudPull(ctx, req)
}

func (l lifecycle) CloudDown(ctx context.Context, req client.CloudDownRequest) error {
	return l.remotes.CloudDown(ctx, req)
}

func (l lifecycle) Diagnose(ctx context.Context, _ client.EndpointID, online bool) ([]manage.DiagnosticRow, error) {
	cfg, err := config.Load(config.ProductDalgona, "", config.Flags{})
	if err != nil {
		return nil, err
	}
	env := doctor.Env{
		Home: cfg.Home, Own: l.backend.Build, Now: time.Now, Out: io.Discard, Online: online,
		Dial: func(context.Context) (net.Conn, error) { return client.DialHome(cfg.Home) },
	}
	rows := doctor.Diagnose(ctx, env)
	out := make([]manage.DiagnosticRow, len(rows))
	for i, r := range rows {
		out[i] = manage.DiagnosticRow{Status: r.Status, Check: r.Check, Detail: r.Detail}
	}
	return out, nil
}

func readPreference(dir, name string) string {
	data, err := os.ReadFile(filepath.Join(dir, name))
	if err != nil && !errors.Is(err, fs.ErrNotExist) {
		slog.Warn("read a saved GUI preference", "file", name, "error", err)
	}
	return strings.TrimSpace(string(data))
}

func writePreference(dir, name, value string) {
	tmp := filepath.Join(dir, name+".tmp")
	if err := os.WriteFile(tmp, []byte(value), 0o600); err != nil {
		slog.Warn("save a GUI preference", "file", name, "error", err)
		return
	}
	if err := os.Rename(tmp, filepath.Join(dir, name)); err != nil {
		slog.Warn("save a GUI preference", "file", name, "error", err)
	}
}

type preference struct {
	dir     string
	name    string
	mu      sync.Mutex
	value   string
	writing sync.Mutex
}

func loadPreference(dir, name string) *preference {
	return &preference{dir: dir, name: name, value: readPreference(dir, name)}
}

func (p *preference) get() string {
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.value
}

func (p *preference) set(value string) {
	p.mu.Lock()
	changed := value != p.value
	p.value = value
	p.mu.Unlock()
	if changed {
		go p.write()
	}
}

func (p *preference) write() {
	p.writing.Lock()
	defer p.writing.Unlock()
	writePreference(p.dir, p.name, p.get())
}

func onOff(on bool) string {
	if on {
		return "on"
	}
	return "off"
}
