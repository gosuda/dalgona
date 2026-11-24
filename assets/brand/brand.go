package brand

import _ "embed"

var (
	//go:embed dalgona.svg
	Mark []byte
	//go:embed dalgona-small.svg
	MarkSmall []byte
)
