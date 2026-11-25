package remote

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"io"
	"strings"

	"github.com/gosuda/dalgona/internal/dalgona/client"
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
	if conflict, ok := errors.AsType[*client.ConflictError](err); ok {
		_, _ = fmt.Fprint(stderr, conflict.Report())
		return 1
	}
	_, _ = fmt.Fprintln(stderr, err)
	return 1
}
