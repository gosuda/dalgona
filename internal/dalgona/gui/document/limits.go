package document

import "fmt"

const (
	MaxMarkdownBytes     = 4 << 20
	MaxMarkdownLineBytes = 64 << 10
	MaxNesting           = 32
	MaxRowBytes          = 64 << 10
	MaxCopyBytes         = 32 << 20
	MaxFindResults       = 10_000
	ResidentSourceBudget = 32 << 20
	GeometryBudget       = 64 << 20
	PageBytes            = 1 << 20
	FollowDistance       = 32
	MaxDragScrollSpeed   = 1600
)

type Limit uint8

const (
	LimitMarkdownBytes Limit = iota + 1
	LimitMarkdownLine
	LimitNesting
	LimitRow
	LimitFindResults
)

type Diagnostic struct {
	Span    Span
	Limit   Limit
	Message string
}

func markdownSizeDiagnostic(size int) Diagnostic {
	return Diagnostic{
		Span:  Span{Start: 0, End: size},
		Limit: LimitMarkdownBytes,
		Message: fmt.Sprintf("This message has %d bytes, above the %d byte Markdown limit, so it shows as literal source.",
			size, MaxMarkdownBytes),
	}
}

func markdownLineDiagnostic(line Span) Diagnostic {
	return Diagnostic{
		Span:  line,
		Limit: LimitMarkdownLine,
		Message: fmt.Sprintf("Line at byte %d has %d bytes, above the %d byte Markdown line limit, so this message shows as literal source.",
			line.Start, line.End-line.Start, MaxMarkdownLineBytes),
	}
}

func nestingDiagnostic(at Span) Diagnostic {
	return Diagnostic{
		Span:  at,
		Limit: LimitNesting,
		Message: fmt.Sprintf("Markdown at byte %d nests deeper than %d levels, so its deeper levels show at level %d.",
			at.Start, MaxNesting, MaxNesting),
	}
}

func inlineNestingDiagnostic(at Span) Diagnostic {
	return Diagnostic{
		Span:  at,
		Limit: LimitNesting,
		Message: fmt.Sprintf("Inline Markdown at byte %d nests deeper than %d levels, so this block shows as literal source.",
			at.Start, MaxNesting),
	}
}

func rowDiagnostic(at Span) Diagnostic {
	return Diagnostic{
		Span:  at,
		Limit: LimitRow,
		Message: fmt.Sprintf("Text at byte %d runs longer than %d bytes without a break, so it wraps at a character boundary.",
			at.Start, MaxRowBytes),
	}
}
