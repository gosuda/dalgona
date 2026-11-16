package prompt

import "strings"

const MemoryHeading = "\n\n## Project memory\n\n"

func MemorySuffix(memory string) string {
	memory = strings.TrimSpace(memory)
	if memory == "" {
		return ""
	}
	return MemoryHeading + memory
}

func AppendMemory(base, memory string) string {
	return base + MemorySuffix(memory)
}
