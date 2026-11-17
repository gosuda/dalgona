//go:build !windows

package battery

import (
	"os"
	"strconv"
)

func daemonUID() string {
	return strconv.Itoa(os.Getuid())
}
