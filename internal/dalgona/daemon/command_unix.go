//go:build !windows

package daemon

import (
	"os"
	"strconv"
)

func serviceUID() string { return strconv.Itoa(os.Getuid()) }
