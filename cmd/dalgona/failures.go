package main

import (
	"bufio"
	"context"
	"fmt"
	"os"
	"strings"
	"time"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dalgona/report"
)

var errNotTerminal = errorString("failure reporting login needs a terminal")

type errorString string

func (e errorString) Error() string { return string(e) }

func runFailures(ctx context.Context, args []string, std stdio) int {
	if len(args) < 1 || args[0] != "reporting" || len(args) != 2 {
		return usage(std, "failures reporting on|off|login")
	}
	switch args[1] {
	case "on":
		return failuresReportingOn(ctx, std)
	case "off":
		return failuresReportingOff(std)
	case "login":
		return failuresReportingLogin(ctx, std)
	}
	return usage(std, "failures reporting on|off|login")
}

func failuresReportingOn(ctx context.Context, std stdio) int {
	cfg, err := loadConfig()
	if err != nil {
		return fail(std, 1, err)
	}
	method := cfg.FailureReports.Auth
	fmt.Fprint(std.out, report.ConsentText(method))
	if !stdinIsTerminal() {
		fmt.Fprintln(std.out, "automatic failure reporting remains off")
		return 0
	}
	line, err := bufio.NewReader(os.Stdin).ReadString('\n')
	if strings.TrimRight(line, "\r\n") != "yes" {
		fmt.Fprintln(std.out, "automatic failure reporting remains off")
		return 0
	}
	if err != nil && line == "" {
		fmt.Fprintln(std.out, "automatic failure reporting remains off")
		return 0
	}
	if err := report.WriteConsent(cfg.Home, method, time.Now()); err != nil {
		return fail(std, 1, err)
	}
	if err := config.WriteSettings(cfg.Home, config.SettingEdit{Key: "failure_reports.enabled", Value: true}); err != nil {
		return fail(std, 1, err)
	}
	return failuresAcquire(ctx, cfg.Home, method, std)
}

func failuresReportingOff(std stdio) int {
	cfg, err := loadConfig()
	if err != nil {
		return fail(std, 1, err)
	}
	if err := config.WriteSettings(cfg.Home, config.SettingEdit{Key: "failure_reports.enabled", Value: false}); err != nil {
		return fail(std, 1, err)
	}
	if err := report.Disable(cfg.Home); err != nil {
		return fail(std, 1, err)
	}
	fmt.Fprintln(std.out, "automatic failure reporting disabled; queued reports deleted")
	fmt.Fprintln(std.out, "published GitHub issues are not deleted")
	return 0
}

func failuresReportingLogin(ctx context.Context, std stdio) int {
	cfg, err := loadConfig()
	if err != nil {
		return fail(std, 1, err)
	}
	return failuresAcquire(ctx, cfg.Home, cfg.FailureReports.Auth, std)
}

func failuresAcquire(ctx context.Context, home, method string, std stdio) int {
	if method == "gh" {
		fmt.Fprintln(std.out, "using the active github.com account from gh; no login, refresh or fallback will run")
		return 0
	}
	if !stdinIsTerminal() {
		return fail(std, 1, errNotTerminal)
	}
	return failuresAppLogin(ctx, home, std)
}

func failuresAppLogin(ctx context.Context, home string, std stdio) int {
	repositoryID, err := report.ResolveReportRepository(ctx)
	if err != nil {
		return fail(std, 1, err)
	}
	err = report.AcquireAppToken(ctx, home, repositoryID, func(code, url string) {
		fmt.Fprintf(std.out, "open %s and enter code %s\n", url, code)
	})
	if err != nil {
		return fail(std, 1, err)
	}
	fmt.Fprintln(std.out, "automatic failure reporting authorized")
	return 0
}
