//go:build gui && guiintegration

package desktop

import (
	"os"
	"testing"

	"gioui.org/app"
)

func TestMain(m *testing.M) {
	go func() { os.Exit(m.Run()) }()
	app.Main()
}
