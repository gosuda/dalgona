package code

import "strings"

var aliases = map[string]string{
	"sh":    "bash",
	"shell": "bash",
	"toml":  "ini",
	"html":  "xml",
	"ts":    "typescript",
	"js":    "javascript",
}

var supported = []string{
	"bash", "css", "diff", "dockerfile", "go", "ini", "javascript",
	"json", "markdown", "python", "rust", "sql", "typescript", "xml", "yaml",
}

func Normalize(name string) (string, bool) {
	canonical := strings.ToLower(strings.TrimSpace(name))
	if mapped, ok := aliases[canonical]; ok {
		canonical = mapped
	}
	for _, known := range supported {
		if canonical == known {
			return known, true
		}
	}
	return "", false
}

func Supported() []string {
	return append([]string(nil), supported...)
}
