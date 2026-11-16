package doctor

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"maps"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"time"

	"github.com/gosuda/dalgona/internal/auth"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/daemon"
	batteryscript "github.com/gosuda/dalgona/internal/dalgona/script"
)

const notChecked = "not checked; the config did not load"

const probeTimeout = 5 * time.Second

const logLines = 20

const logTail = 64 << 10

const threadIDs = 5

const loadEffectMessage = "effects are not allowed at load time"

const inspectionMessage = "effects are not available during inspection"

type joinedError interface {
	error
	Unwrap() []error
}

func (in *inspector) checkHome(context.Context) []row {
	if err := daemon.VerifyHome(in.env.Home); err != nil {
		return []row{failRow("home", err.Error())}
	}
	return []row{okRow("home", in.env.Home)}
}

func (in *inspector) checkConfig(context.Context) []row {
	cwd, err := os.Getwd()
	if err != nil {
		cwd = ""
	}
	cfg, err := config.Load(config.ProductDalgona, cwd, config.Flags{})
	if err != nil {
		return []row{failRow("config", err.Error())}
	}
	in.cfg, in.cfgLoaded = cfg, true
	return []row{okRow("config", "valid")}
}

func (in *inspector) checkDaemon(ctx context.Context) []row {
	conn, err := in.env.Dial(ctx)
	if err != nil {
		return in.dialFailed(err)
	}
	probeCtx, cancel := context.WithTimeout(ctx, probeTimeout)
	defer cancel()
	info, probe, err := daemon.ProbeConn(probeCtx, conn)
	if err != nil {
		rows := []row{
			failRow("daemon", "cannot inspect the running daemon: "+err.Error()),
			warnRow("build", "not checked; the daemon cannot be inspected"),
		}
		return append(rows, in.daemonLog()...)
	}
	if probe == daemon.ProbePreIdentity {
		return []row{okRow("daemon", "running; it reports no identity"), in.buildRow(daemon.UnknownPreIdentityBuild)}
	}
	return []row{okRow("daemon", in.running(info)), in.buildRow(info.Build)}
}

func (in *inspector) dialFailed(err error) []row {
	switch {
	case errors.Is(err, fs.ErrNotExist):
		return []row{okRow("daemon", "not running; it starts on demand"), in.buildRow(in.env.Own)}
	case daemon.Untrusted(err):
		rows := []row{
			failRow("daemon", "the socket belongs to another user: "+err.Error()),
			warnRow("build", "not checked; the socket is not trusted"),
		}
		return append(rows, in.daemonLog()...)
	case !socketExists(daemon.SocketPath(in.env.Home)):
		return []row{
			failRow("daemon", "cannot reach the socket: "+err.Error()),
			warnRow("build", "not checked; the socket cannot be reached"),
		}
	}
	stale := fmt.Sprintf("stale socket %s; any dalgona command replaces it", daemon.SocketPath(in.env.Home))
	rows := []row{warnRow("daemon", stale), in.buildRow(in.env.Own)}
	return append(rows, in.daemonLog()...)
}

func socketExists(path string) bool {
	_, err := os.Lstat(path)
	return err == nil
}

func (in *inspector) running(info daemon.Info) string {
	text := fmt.Sprintf("running pid %d", info.PID)
	if info.StartedAt.IsZero() {
		return text
	}
	return text + " up " + formatDuration(max(0, in.env.Now().Sub(info.StartedAt)))
}

func (in *inspector) buildRow(running string) row {
	own := in.env.Own
	switch {
	case own == "":
		return warnRow("build", "this binary's build id is unavailable")
	case running == "":
		return warnRow("build", "the daemon reports no build id")
	case running != own:
		return failRow("build", fmt.Sprintf("daemon runs build %s, this binary is build %s; run dalgona restart", running, own))
	}
	return okRow("build", own)
}

func (in *inspector) daemonLog() []row {
	lines, err := tailLines(filepath.Join(in.env.Home, "daemon.log"), logLines)
	if err != nil {
		return []row{warnRow("daemon-log", err.Error())}
	}
	if len(lines) == 0 {
		return []row{warnRow("daemon-log", "daemon.log has no lines")}
	}
	rows := make([]row, 0, len(lines))
	for _, line := range lines {
		rows = append(rows, warnRow("daemon-log", line))
	}
	return rows
}

func tailLines(path string, count int) ([]string, error) {
	file, err := os.Open(path)
	if err != nil {
		return nil, fmt.Errorf("read daemon.log: %w", err)
	}
	defer file.Close()
	info, err := file.Stat()
	if err != nil {
		return nil, fmt.Errorf("read daemon.log: %w", err)
	}
	offset := max(0, info.Size()-logTail)
	data, err := io.ReadAll(io.NewSectionReader(file, offset, logTail))
	if err != nil {
		return nil, fmt.Errorf("read daemon.log: %w", err)
	}
	text := string(data)
	if offset > 0 {
		_, text, _ = strings.Cut(text, "\n")
	}
	var lines []string
	for line := range strings.SplitSeq(text, "\n") {
		if strings.TrimSpace(line) != "" {
			lines = append(lines, line)
		}
	}
	return lines[max(0, len(lines)-count):], nil
}

func (in *inspector) checkThreads(context.Context) []row {
	total, damaged, err := countThreads(filepath.Join(in.env.Home, "sessions"))
	if err != nil {
		return []row{failRow("threads", err.Error())}
	}
	if len(damaged) == 0 {
		return []row{okRow("threads", fmt.Sprintf("%d threads", total))}
	}
	shown := strings.Join(damaged[:min(len(damaged), threadIDs)], ", ")
	return []row{warnRow("threads", fmt.Sprintf("%d threads, %d damaged: %s", total, len(damaged), shown))}
}

func countThreads(root string) (int, []string, error) {
	entries, err := os.ReadDir(root)
	if errors.Is(err, fs.ErrNotExist) {
		return 0, nil, nil
	}
	if err != nil {
		return 0, nil, fmt.Errorf("read sessions: %w", err)
	}
	total := 0
	var damaged []string
	for _, entry := range entries {
		dir := filepath.Join(root, entry.Name())
		if !entry.IsDir() || !isFile(filepath.Join(dir, "journal.jsonl")) {
			continue
		}
		total++
		if isFile(filepath.Join(dir, "damaged")) {
			damaged = append(damaged, entry.Name())
		}
	}
	return total, damaged, nil
}

func isFile(path string) bool {
	info, err := os.Stat(path)
	return err == nil && info.Mode().IsRegular()
}

func (in *inspector) checkProviders(ctx context.Context) []row {
	if in.store == nil {
		return []row{failRow("providers", in.storeErr.Error())}
	}
	in.configured = nil
	var rows []row
	for _, id := range in.providerIDs() {
		cred, err := in.store.TokenWithEnv(ctx, id, in.cfg.Providers[id].APIKeyEnv)
		if errors.Is(err, auth.ErrNoCredential) {
			continue
		}
		if err != nil {
			rows = append(rows, failRow("providers", id+": "+err.Error()))
			continue
		}
		in.configured = append(in.configured, configured{id: id, cred: cred})
		rows = append(rows, credentialRow(id, cred, in.env.Now()))
	}
	if len(rows) == 0 {
		return []row{warnRow("providers", "no credential found; run dalgona login <id>")}
	}
	return rows
}

func (in *inspector) providerIDs() []string {
	ids := slices.Concat(auth.Providers(), slices.Collect(maps.Keys(in.cfg.Providers)))
	slices.Sort(ids)
	return slices.Compact(ids)
}

func credentialRow(id string, cred auth.Credential, now time.Time) row {
	if cred.Type == "api_key" || (cred.Access == "" && cred.Refresh == "") {
		return apiKeyRow(id, cred)
	}
	return tokenRow(id, cred, now)
}

func apiKeyRow(id string, cred auth.Credential) row {
	if cmp.Or(cred.Key, cred.Access) == "" {
		return failRow("providers", fmt.Sprintf("%s: api key is empty; run dalgona login %s", id, id))
	}
	return okRow("providers", id+": api key set")
}

func tokenRow(id string, cred auth.Credential, now time.Time) row {
	expires, err := parseExpiry(cred.Expires)
	switch {
	case err != nil:
		return failRow("providers", fmt.Sprintf("%s: expiry %q is not valid; run dalgona login %s", id, cred.Expires, id))
	case cred.Access == "" || (!expires.IsZero() && !now.Before(expires)):
		return expiredRow(id, cred)
	case expires.IsZero():
		return okRow("providers", id+": token set, no expiry recorded")
	}
	return okRow("providers", fmt.Sprintf("%s: expires in %s", id, formatDuration(expires.Sub(now))))
}

func expiredRow(id string, cred auth.Credential) row {
	if cred.Refresh != "" {
		return warnRow("providers", id+": expired, refreshes on next use")
	}
	return failRow("providers", fmt.Sprintf("%s: expired; run dalgona login %s", id, id))
}

func parseExpiry(text string) (time.Time, error) {
	if text == "" {
		return time.Time{}, nil
	}
	expires, err := time.Parse(time.RFC3339, text)
	if err != nil {
		return time.Time{}, fmt.Errorf("parse expiry: %w", err)
	}
	return expires, nil
}

func (in *inspector) checkSandbox(context.Context) []row {
	if in.cfgLoaded && in.cfg.Sandbox == config.SandboxOff {
		return []row{okRow("sandbox", "off by configuration")}
	}
	if cause := in.sandboxCause(); cause != "" {
		return []row{warnRow("sandbox", cause+"; commands ask for a sandbox choice")}
	}
	return []row{okRow("sandbox", harnessName(in.goos))}
}

func harnessName(goos string) string {
	switch goos {
	case "linux":
		return "Landlock"
	case "darwin":
		return "sandbox-exec"
	}
	return goos + " sandbox"
}

func (in *inspector) checkTLS(context.Context) []row {
	if !in.cfgLoaded {
		return []row{warnRow("tls", notChecked)}
	}
	if in.cfg.TLSCAFile == "" {
		return []row{okRow("tls", "system roots")}
	}
	count, err := certificateCount(in.cfg.TLSCAFile)
	if err != nil {
		return []row{failRow("tls", err.Error())}
	}
	return []row{okRow("tls", fmt.Sprintf("%d certificates from %s", count, in.cfg.TLSCAFile))}
}

func (in *inspector) checkMCP(context.Context) []row {
	if !in.cfgLoaded {
		return []row{warnRow("mcp", notChecked)}
	}
	if len(in.cfg.MCPServers) == 0 {
		return []row{okRow("mcp", "none configured")}
	}
	rows := make([]row, 0, len(in.cfg.MCPServers))
	for _, name := range slices.Sorted(maps.Keys(in.cfg.MCPServers)) {
		rows = append(rows, okRow("mcp", name+": "+transport(in.cfg.MCPServers[name])))
	}
	return rows
}

func transport(spec config.MCPServerSpec) string {
	if spec.URL != "" {
		return "http"
	}
	return "stdio"
}

func (in *inspector) checkPlugins(context.Context) []row {
	files, err := filepath.Glob(filepath.Join(in.env.Home, "plugins", "*.star"))
	if err != nil {
		return []row{failRow("plugins", fmt.Sprintf("list plugins: %v", err))}
	}
	_, loadErr := batteryscript.LoadPlugins(in.env.Home)
	failures := loadFailures(loadErr)
	if len(failures) == 0 {
		return []row{okRow("plugins", fmt.Sprintf("%d loaded", len(files)))}
	}
	rows := make([]row, 0, len(failures))
	for _, failure := range failures {
		rows = append(rows, failRow("plugins", strings.Replace(failure.Error(), loadEffectMessage, inspectionMessage, 1)))
	}
	return rows
}

func loadFailures(err error) []error {
	if err == nil {
		return nil
	}
	if joined, ok := errors.AsType[joinedError](err); ok {
		return joined.Unwrap()
	}
	return []error{err}
}
