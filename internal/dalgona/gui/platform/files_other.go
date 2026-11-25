//go:build !windows

package platform

func localPath(path string) (string, error) {
	return path, nil
}
