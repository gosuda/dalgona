//go:build linux

package runner

func selfExe() (string, error) {
	return "/proc/self/exe", nil
}
