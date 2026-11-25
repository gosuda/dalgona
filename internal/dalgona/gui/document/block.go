package document

import (
	"errors"
	"fmt"
	"net/url"
	"strings"
)

type Kind uint8

const (
	KindParagraph Kind = iota + 1
	KindHeading
	KindCode
	KindTable
	KindRule
	KindHTML
	KindFigure
	KindMath
	KindNotice
	KindDiff
)

type ContainerKind uint8

const (
	ContainerQuotation ContainerKind = iota + 1
	ContainerListItem
)

type Container struct {
	Kind    ContainerKind
	Ordered bool
	Tight   bool
	First   bool
	Marker  byte
	Start   int
	Number  int
	id      int
}

type Task uint8

const (
	TaskNone Task = iota
	TaskOpen
	TaskDone
)

type Alignment uint8

const (
	AlignNone Alignment = iota
	AlignLeft
	AlignCenter
	AlignRight
)

type Link struct {
	Destination string
	Title       string
	Reference   string
}

type Block struct {
	Key         Key
	Kind        Kind
	Span        Span
	Level       int
	Language    string
	Containers  []Container
	Task        Task
	Alignments  []Alignment
	Links       []Link
	Projection  Projection
	Body        Span
	Rows        []Span
	Fingerprint uint64
	Math        Span
}

var ErrLink = errors.New("link cannot open")

var openableSchemes = map[string]bool{"https": true, "http": true, "mailto": true}

func (l Link) Target() (string, error) {
	parsed, err := url.Parse(l.Destination)
	if err != nil {
		return "", fmt.Errorf("%w: %q is not a valid address; copy the text instead", ErrLink, l.Destination)
	}
	scheme := strings.ToLower(parsed.Scheme)
	if !openableSchemes[scheme] {
		return "", fmt.Errorf("%w: %q does not use https, http, or mailto; only those links open", ErrLink, l.Destination)
	}
	if scheme != "mailto" && parsed.Host == "" {
		return "", fmt.Errorf("%w: %q has no host; copy the text instead", ErrLink, l.Destination)
	}
	if scheme == "mailto" && parsed.Opaque == "" {
		return "", fmt.Errorf("%w: %q has no address; copy the text instead", ErrLink, l.Destination)
	}
	return parsed.String(), nil
}
