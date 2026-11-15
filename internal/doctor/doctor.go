package doctor

import (
	"context"
	"fmt"
	"io"
	"net"
	"runtime"
	"strings"
	"time"
	"unicode"

	"github.com/gosuda/dalgona/internal/auth"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/runner"
)

const (
	statusOK   = "ok"
	statusWarn = "warn"
	statusFail = "fail"

	maxDetail = 400
)

type Env struct {
	Home   string
	Dial   func(context.Context) (net.Conn, error)
	Own    string
	Now    func() time.Time
	Out    io.Writer
	Online bool
}

type row struct{ status, check, detail string }

type configured struct {
	id   string
	cred auth.Credential
}

type inspector struct {
	env          Env
	cfg          config.Config
	cfgLoaded    bool
	store        *auth.Store
	storeErr     error
	configured   []configured
	sandboxCause func() string
	goos         string
	timeout      time.Duration
}

func Run(ctx context.Context, env Env) int {
	return newInspector(env).run(ctx)
}

func newInspector(env Env) *inspector {
	if env.Now == nil {
		env.Now = time.Now
	}
	if env.Out == nil {
		env.Out = io.Discard
	}
	in := &inspector{env: env, sandboxCause: runner.UnavailableCause, goos: runtime.GOOS, timeout: modelsTimeout}
	in.store, in.storeErr = auth.Load(env.Home)
	return in
}

func (in *inspector) checks() []func(context.Context) []row {
	checks := []func(context.Context) []row{in.checkHome, in.checkConfig, in.checkDaemon, in.checkThreads, in.checkProviders}
	if in.env.Online {
		checks = append(checks, in.checkModels)
	}
	return append(checks, in.checkSandbox, in.checkTLS, in.checkMCP, in.checkPlugins)
}

func (in *inspector) run(ctx context.Context) int {
	failed := false
	for _, check := range in.checks() {
		for _, r := range check(ctx) {
			fmt.Fprintf(in.env.Out, "%s %s: %s\n", r.status, r.check, in.clean(r.detail))
			failed = failed || r.status == statusFail
		}
	}
	if failed {
		return 1
	}
	return 0
}

func okRow(check, detail string) row { return row{statusOK, check, detail} }

func warnRow(check, detail string) row { return row{statusWarn, check, detail} }

func failRow(check, detail string) row { return row{statusFail, check, detail} }

func (in *inspector) clean(text string) string {
	if in.store != nil {
		text = in.store.Mask(text)
	}
	return clip(oneLine(text))
}

func oneLine(text string) string {
	plain := strings.Map(func(r rune) rune {
		if unicode.IsControl(r) || unicode.Is(unicode.Cf, r) {
			return ' '
		}
		return r
	}, text)
	return strings.Join(strings.Fields(plain), " ")
}

func clip(text string) string {
	runes := []rune(text)
	if len(runes) <= maxDetail {
		return text
	}
	return string(runes[:maxDetail]) + "..."
}

func formatDuration(d time.Duration) string {
	switch {
	case d < time.Second:
		return fmt.Sprintf("%d ms", d.Milliseconds())
	case d < time.Minute:
		return fmt.Sprintf("%ds", int64(d/time.Second))
	case d < time.Hour:
		return fmt.Sprintf("%dm %02ds", int64(d/time.Minute), int64(d%time.Minute/time.Second))
	}
	return fmt.Sprintf("%dh %02dm", int64(d/time.Hour), int64(d%time.Hour/time.Minute))
}
