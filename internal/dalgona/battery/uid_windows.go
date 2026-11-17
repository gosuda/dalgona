//go:build windows

package battery

func daemonUID() string {
	return "0"
}
