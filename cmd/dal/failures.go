package main

import (
	"cmp"
	"context"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"strings"
	"time"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/failure"
)

const failuresUsage = "usage: dal failures [id]"

func runFailures(ctx context.Context, home string, args []string, stdout, stderr io.Writer) int {
	if len(args) > 1 {
		fmt.Fprintln(stderr, failuresUsage)
		return 2
	}
	store, err := failure.Open(ctx, failure.Options{Home: home, Product: string(config.ProductDal), Version: config.Version()})
	if err != nil {
		fmt.Fprintf(stderr, "%s: %v\n", binary, err)
		return 1
	}
	if len(args) == 0 {
		err = listFailures(ctx, store, stdout)
	} else {
		err = printFailure(ctx, store, args[0], stdout)
	}
	if err != nil {
		fmt.Fprintf(stderr, "%s: %v\n", binary, err)
		return 1
	}
	return 0
}

func listFailures(ctx context.Context, store *failure.Store, stdout io.Writer) error {
	records, err := store.List(ctx, failure.Query{})
	if err != nil {
		return err
	}
	if len(records) == 0 {
		_, err = fmt.Fprintln(stdout, "no failures recorded")
		return err
	}
	var out strings.Builder
	for _, record := range records {
		fmt.Fprintf(&out, "%s  %s  %s  %s\n", record.ID, record.Time.UTC().Format(time.RFC3339), record.Class, cmp.Or(record.Tool, "runtime"))
	}
	out.WriteString("run dal failures <id> to print a report you can file by hand\n")
	_, err = io.WriteString(stdout, out.String())
	return err
}

func printFailure(ctx context.Context, store *failure.Store, id string, stdout io.Writer) error {
	if !validFailureID(id) {
		return errors.New("failure id must be 32 lowercase hexadecimal digits; run dal failures to list them")
	}
	records, err := store.List(ctx, failure.Query{ID: id})
	if err != nil {
		return err
	}
	_, err = io.WriteString(stdout, failure.ReportText(records[0]))
	return err
}

func validFailureID(id string) bool {
	_, err := hex.DecodeString(id)
	return err == nil && len(id) == 32 && strings.ToLower(id) == id
}
