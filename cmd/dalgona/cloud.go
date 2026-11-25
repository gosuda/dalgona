package main

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"strings"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/sandboxsvc/registry"
)

const cloudUsage = "usage: dalgona cloud up <name> [--copy-auth] | down <name> [--yes] | pull <name>"

type cloudCommand struct{ verb, name, flag string }

var cloudFlags = map[string]string{"up": "--copy-auth", "down": "--yes", "pull": ""}

func runCloud(ctx context.Context, args []string, std stdio) int {
	command, ok := parseCloud(args)
	if !ok {
		fmt.Fprintln(std.errOut, cloudUsage)
		return 2
	}
	cfg, err := loadConfig()
	if err != nil {
		return fail(std, 1, err)
	}
	store, err := authStore(cfg)
	if err != nil {
		return fail(std, 1, err)
	}
	services := registry.New(registry.Options{Auth: store, Logger: slog.New(slog.NewTextHandler(std.errOut, nil))})
	err = command.run(ctx, client.NewRemotes(cfg, services), cfg.Cwd, std)
	if conflict, ok := errors.AsType[*client.ConflictError](err); ok {
		fmt.Fprint(std.errOut, conflict.Report())
		return 1
	}
	if err != nil {
		fmt.Fprintln(std.errOut, err)
		return 1
	}
	return 0
}

func parseCloud(args []string) (cloudCommand, bool) {
	if len(args) < 2 {
		return cloudCommand{}, false
	}
	allowed, known := cloudFlags[args[0]]
	command := cloudCommand{verb: args[0]}
	for _, arg := range args[1:] {
		if !command.take(arg, allowed) {
			return cloudCommand{}, false
		}
	}
	return command, known && command.name != ""
}

func (c *cloudCommand) take(arg, allowedFlag string) bool {
	switch {
	case allowedFlag != "" && arg == allowedFlag && c.flag == "":
		c.flag = arg
	case c.name == "" && !strings.HasPrefix(arg, "-"):
		c.name = arg
	default:
		return false
	}
	return true
}

func (c cloudCommand) run(ctx context.Context, remotes *client.Remotes, cwd string, std stdio) error {
	request := client.CloudRequest{Endpoint: client.EndpointID(c.name), OperationID: rpc.NewOperationID()}
	switch c.verb {
	case "up":
		return cloudUp(ctx, remotes, client.CloudUpRequest{CloudRequest: request, Workspace: cwd, CopyAuth: c.flag != ""}, std)
	case "down":
		return cloudDown(ctx, remotes, request, c.flag != "", std)
	}
	changed, err := remotes.CloudPull(ctx, client.CloudPullRequest{CloudRequest: request, Workspace: cwd})
	if err == nil && !changed {
		fmt.Fprintln(std.out, "nothing to pull")
	}
	return err
}

func cloudUp(ctx context.Context, remotes *client.Remotes, req client.CloudUpRequest, std stdio) error {
	if !req.CopyAuth {
		fmt.Fprintf(std.out, "sign in inside the box with `dalgona --remote %s login`\n", req.Endpoint)
	}
	box, err := remotes.CloudUp(ctx, req)
	if err != nil {
		return err
	}
	fmt.Fprintf(std.out, "remote %s: %s box %s\n", req.Endpoint, box.Service, box.BoxID)
	return nil
}

func cloudDown(ctx context.Context, remotes *client.Remotes, req client.CloudRequest, yes bool, std stdio) error {
	spec, _, err := remotes.Boxed(ctx, string(req.Endpoint))
	if err != nil {
		return err
	}
	if !yes {
		fmt.Fprintf(std.out, "destroy %s box %s and every thread in it? [y/N] ", spec.Service, spec.BoxID)
		answer, readErr := bufio.NewReader(std.in).ReadString('\n')
		if readErr != nil && !errors.Is(readErr, io.EOF) {
			return readErr
		}
		if answer = strings.ToLower(strings.TrimSpace(answer)); answer != "y" && answer != "yes" {
			return errors.New("remote: cloud down cancelled")
		}
	}
	return remotes.CloudDown(ctx, client.CloudDownRequest{CloudRequest: req, Box: spec.BoxID})
}
