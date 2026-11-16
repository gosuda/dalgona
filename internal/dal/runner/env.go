//go:build !windows

package runner

import "strings"

func setEnvironment(env []string, key string, value string) []string {
	result := removeEnvironment(env, key)
	return append(result, key+"="+value)
}

func removeEnvironment(env []string, key string) []string {
	result := make([]string, 0, len(env)+1)
	for _, item := range env {
		name, _, ok := strings.Cut(item, "=")
		if ok && name == key {
			continue
		}
		result = append(result, item)
	}
	return result
}
