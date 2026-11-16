//go:build !unix

package patch

import "github.com/gosuda/dalgona/internal/tool"

type dirPin = rootPin

func pinVerifiedComponents(env tool.Env, display, absolute string) (*dirPin, error) {
	return pinRoot(env, display, absolute)
}
