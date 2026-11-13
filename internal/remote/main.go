package remote

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"io"
	"strings"
)

type IO struct {
	Stdin          io.Reader
	Stdout, Stderr io.Writer
}

func PullMain(ctx context.Context, opts Options, args []string, stdio IO) int {
	if len(args) != 1 {
		_, _ = fmt.Fprintln(stdio.Stderr, "usage: dalgona pull <thread>")
		return 2
	}
	return report(stdio.Stderr, cliBinder(opts, stdio).Pull(ctx, args[0]))
}

func RunnerMain(ctx context.Context, opts Options, args []string, stdio IO) int {
	if len(args) != 1 || args[0] != "gc" {
		_, _ = fmt.Fprintln(stdio.Stderr, "usage: dalgona runner gc")
		return 2
	}
	boxes, err := cliBinder(opts, stdio).GC(ctx)
	if err == nil && len(boxes) == 0 {
		_, _ = fmt.Fprintln(stdio.Stdout, "no unbound boxes")
	}
	return report(stdio.Stderr, err)
}

func CloudMain(ctx context.Context, opts Options, args []string, stdio IO) int {
	command, ok := parseCloud(args)
	if !ok {
		_, _ = fmt.Fprintln(stdio.Stderr, "usage: dalgona cloud up <name> [--copy-auth] | down <name> [--yes] | pull <name>")
		return 2
	}
	b := cliBinder(opts, stdio)
	switch command.verb {
	case "up":
		if command.flag == "" {
			_, _ = fmt.Fprintf(stdio.Stdout, "sign in inside the box with `dalgona --remote %s login`\n", command.name)
		}
		return report(stdio.Stderr, b.CloudUp(ctx, command.name, command.flag != ""))
	case "down":
		return report(stdio.Stderr, b.CloudDown(ctx, command.name, command.flag != ""))
	default:
		return report(stdio.Stderr, b.CloudPull(ctx, command.name))
	}
}

type cloudCommand struct{ verb, name, flag string }

var cloudFlags = map[string]string{"up": "--copy-auth", "down": "--yes", "pull": ""}

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

func cliBinder(opts Options, stdio IO) *Binder {
	if opts.Out == nil {
		opts.Out = stdio.Stdout
	}
	if opts.Confirm == nil {
		opts.Confirm = promptConfirm(stdio.Stdin, stdio.Stdout)
	}
	return NewBinder(opts)
}

func promptConfirm(stdin io.Reader, stdout io.Writer) func(string) (bool, error) {
	reader := bufio.NewReader(stdin)
	return func(prompt string) (bool, error) {
		_, _ = fmt.Fprintf(stdout, "%s [y/N] ", prompt)
		line, err := reader.ReadString('\n')
		if err != nil && !errors.Is(err, io.EOF) {
			return false, err
		}
		answer := strings.ToLower(strings.TrimSpace(line))
		return answer == "y" || answer == "yes", nil
	}
}

func report(stderr io.Writer, err error) int {
	if err == nil {
		return 0
	}
	if conflict, ok := errors.AsType[*ConflictError](err); ok {
		_, _ = fmt.Fprintln(stderr, "remote: pull left conflict markers in:")
		for _, file := range conflict.Files {
			_, _ = fmt.Fprintln(stderr, "  "+file)
		}
		return 1
	}
	_, _ = fmt.Fprintln(stderr, err)
	return 1
}
