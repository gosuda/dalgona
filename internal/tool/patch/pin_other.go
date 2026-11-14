//go:build !unix

package patch

type dirPin = rootPin

var pinVerifiedComponents = pinRoot
