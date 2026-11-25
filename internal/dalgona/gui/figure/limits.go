package figure

import (
	"time"

	"github.com/gosuda/dalgona/internal/dalgona/gui/clipboardimage"
)

const (
	SourceLimit          = 64 << 10
	HighlightSourceLimit = 4 << 20
	EngineOutputLimit    = 16 << 20
	GraphNodeLimit       = 1000
	GraphEdgeLimit       = 2000
	SVGDepthLimit        = 64
	SVGElementLimit      = 50_000
	StateDepthLimit      = 256
	OperationLimit       = 200_000
	ResultByteLimit      = 16 << 20
	ReplyFrameLimit      = 1 << 20
	EnvelopeLimit        = 4096
	TileSideLimit        = 256
	TileByteLimit        = 256 << 10
	QuickJSHeapLimit     = 128 << 20
	QuickJSStackSlots    = 2048
	ChildMemoryLimit     = 512 << 20
	CoordinateLimit      = 1_000_000
	PendingJobLimit      = 8
	FailureWindowLimit   = 3
	PixelCacheLimit      = 64 << 20
)

func resultByteLimit(kind string) int {
	switch kind {
	case resultImage:
		return PixelCacheLimit
	case resultClipboardImage:
		return clipboardimage.MaxBytes
	}
	return ResultByteLimit
}

const (
	RenderDeadline     = 2 * time.Second
	TerminationGrace   = 250 * time.Millisecond
	MemorySamplePeriod = 50 * time.Millisecond
	FailureWindow      = 60 * time.Second
)
