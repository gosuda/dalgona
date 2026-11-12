package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"os/signal"
	"syscall"
	"time"

	"github.com/gosuda/dalgona/internal/llmstub"
)

const (
	defaultListen = "127.0.0.1:18080"
	headerLimit   = 10 * time.Second
	shutdownLimit = 5 * time.Second
)

type config struct {
	listen string
	script string
	key    string
	record string
}

func main() {
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	code := run(ctx, os.Args[1:], os.Stderr)
	stop()
	os.Exit(code)
}

func run(ctx context.Context, args []string, stderr io.Writer) int {
	cfg, err := parseArgs(args)
	if err != nil {
		fmt.Fprintf(stderr, "llmstub: %v\n", err)
		return 2
	}
	if err = serve(ctx, cfg, stderr); err != nil {
		fmt.Fprintf(stderr, "llmstub: %v\n", err)
		return 1
	}
	return 0
}

func parseArgs(args []string) (config, error) {
	var cfg config
	flags := flag.NewFlagSet("llmstub", flag.ContinueOnError)
	flags.SetOutput(io.Discard)
	flags.StringVar(&cfg.listen, "listen", defaultListen, "address to serve on, host:port")
	flags.StringVar(&cfg.script, "script", "", "script JSON file")
	flags.StringVar(&cfg.key, "key", "", "bearer token the server accepts")
	flags.StringVar(&cfg.record, "record", "", "file that receives one JSON line for each request")
	if err := flags.Parse(args); err != nil {
		return cfg, err
	}
	if cfg.script == "" {
		return cfg, errors.New("-script is required")
	}
	if cfg.key == "" {
		return cfg, errors.New("-key is required")
	}
	return cfg, nil
}

func serve(ctx context.Context, cfg config, stderr io.Writer) (err error) {
	script, err := loadScript(cfg.script)
	if err != nil {
		return err
	}
	record, closeRecord, err := openRecord(cfg.record)
	if err != nil {
		return err
	}
	defer func() { err = errors.Join(err, closeRecord()) }()
	listener, err := net.Listen("tcp", cfg.listen)
	if err != nil {
		return err
	}
	fmt.Fprintf(stderr, "llmstub: serving http://%s/v1\n", listener.Addr())
	server := &http.Server{
		Handler:           llmstub.NewHandler(script, llmstub.Options{Key: cfg.key, Record: record, Log: stderr}),
		ReadHeaderTimeout: headerLimit,
	}
	return wait(ctx, server, listener)
}

func loadScript(path string) (llmstub.Script, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		return llmstub.Script{}, err
	}
	return llmstub.Parse(path, raw)
}

func openRecord(path string) (io.Writer, func() error, error) {
	if path == "" {
		return nil, func() error { return nil }, nil
	}
	file, err := os.OpenFile(path, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o600)
	if err != nil {
		return nil, nil, err
	}
	return file, file.Close, nil
}

func wait(ctx context.Context, server *http.Server, listener net.Listener) error {
	served := make(chan error, 1)
	go func() { served <- server.Serve(listener) }()
	select {
	case err := <-served:
		return err
	case <-ctx.Done():
	}
	shutdown, cancel := context.WithTimeout(context.Background(), shutdownLimit)
	defer cancel()
	return server.Shutdown(shutdown)
}
