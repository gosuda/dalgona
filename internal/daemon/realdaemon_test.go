package daemon

import (
	"context"
	"fmt"
	"os"
	"os/signal"
	"path/filepath"
	"strconv"
	"syscall"

	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/rpc"
)

const realDaemonEnv = "DALGONA_TEST_REAL_DAEMON"

func runRealDaemon() int {
	home := config.ProductDalgona.Home()
	pid := []byte(strconv.Itoa(os.Getpid()))
	if err := os.WriteFile(filepath.Join(home, "daemon.pid"), pid, 0o600); err != nil {
		fmt.Fprintln(os.Stderr, err)
		return 1
	}
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	if err := Run(ctx, home, appHost, rpc.Serve); err != nil {
		fmt.Fprintln(os.Stderr, err)
		return 1
	}
	return 0
}
