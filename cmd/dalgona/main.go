package main

import (
	"context"
	"fmt"
	"io"
	"os"
	"os/signal"
	"slices"
	"strings"
	"syscall"

	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool/search"
	"github.com/gosuda/dalgona/internal/tool/webfetch"
)

const binary = "dalgona"

type stdio struct {
	in          io.Reader
	out, errOut io.Writer
	remote      string
}

func (s stdio) Read(p []byte) (int, error)  { return s.in.Read(p) }
func (s stdio) Write(p []byte) (int, error) { return s.out.Write(p) }

func main() {
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	code := run(ctx, os.Args[1:], stdio{in: os.Stdin, out: os.Stdout, errOut: os.Stderr})
	stop()
	os.Exit(code)
}

func run(ctx context.Context, args []string, std stdio) int {
	if len(args) > 0 {
		if command, ok := hidden[args[0]]; ok {
			return command(args[1:], std)
		}
	}
	name, args, err := splitRemoteFlag(args)
	if err != nil {
		return fail(std, 2, err)
	}
	std.remote = name
	if len(args) == 1 && args[0] == "--version" {
		fmt.Fprintf(std.out, "%s %s\n", binary, config.Version())
		return 0
	}
	command, rest := dispatch(args)
	return command(ctx, rest, std)
}

func dispatch(args []string) (func(context.Context, []string, stdio) int, []string) {
	if pairs, rest := leadingConfig(args); len(rest) > 0 && rest[0] == "app-server" {
		return runAppServer, append(pairs, rest[1:]...)
	}
	if len(args) > 0 {
		if command, ok := commands[args[0]]; ok {
			return command, args[1:]
		}
	}
	return runClient, args
}

func leadingConfig(args []string) ([]string, []string) {
	i := 0
	for i < len(args) {
		if value, inline := strings.CutPrefix(args[i], "--config="); inline && strings.Contains(value, "=") {
			i++
			continue
		}
		isFlag := args[i] == "--config" || args[i] == "-c"
		if !isFlag || i+1 >= len(args) || !strings.Contains(args[i+1], "=") {
			break
		}
		i += 2
	}
	return slices.Clone(args[:i]), args[i:]
}

var hidden = map[string]func([]string, stdio) int{
	"__shim":   func(args []string, _ stdio) int { return runner.ShimMain(args) },
	"__search": func(args []string, std stdio) int { return search.Main(args, std.out) },
	"__fetch":  func(args []string, std stdio) int { return webfetch.Main(args, std.out) },
}

var commands = map[string]func(context.Context, []string, stdio) int{
	"daemon":      runDaemon,
	"service":     runService,
	"bridge":      runBridge,
	"exec-server": runExecServer,
	"app-server":  runAppServer,
	"codex":       runCodex,
	"web":         runWeb,
	"pull":        runPull,
	"runner":      runRunner,
	"cloud":       runCloud,
	"run":         runWorkflow,
}

func fail(std stdio, code int, err error) int {
	fmt.Fprintf(std.errOut, "%s: %v\n", binary, err)
	return code
}
