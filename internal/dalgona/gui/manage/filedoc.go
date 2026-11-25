//go:build gui

package manage

import (
	"bytes"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"errors"
	"fmt"
	"image"
	_ "image/gif"
	_ "image/jpeg"
	_ "image/png"
	"path"
	"slices"
	"strconv"
	"strings"
	"unicode/utf8"

	"gioui.org/layout"
	"gioui.org/op/paint"
	"gioui.org/unit"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/textinput"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

const (
	editLimit         = 1 << 20
	imageLimit        = 8 << 20
	transferLimit     = 64 << 20
	chunkLimit        = 1 << 20
	absent            = "absent"
	maxImageDimension = 16384
	maxImagePixels    = 32 << 20
)

var (
	documentPolicy   = textinput.Policy{Lines: textinput.MultiLine, Limit: textinput.Limit{Unit: textinput.Bytes, Max: editLimit}}
	errTooLarge      = errors.New("the file exceeds the 64 MiB transfer limit")
	errReadRevision  = errors.New("the daemon changed the file revision during the read")
	errImageTooLarge = errors.New("the image exceeds the 32-megapixel viewer limit")
)

type docKind uint8

const (
	docNone docKind = iota
	docText
	docImage
	docBinary
	docUndecodable
	docTooLarge
)

type loadedFile struct {
	Revision string
	Total    uint64
	Data     []byte
}

type conflict struct {
	path    string
	meta    *fileMetadata
	theirs  *loadedFile
	err     error
	loading bool
}

type document struct {
	path       string
	meta       *fileMetadata
	loaded     loadedFile
	kind       docKind
	loading    bool
	err        error
	text       *field
	img        paint.ImageOp
	imgSize    image.Point
	search     *field
	findNext   widget.Clickable
	saveBtn    widget.Clickable
	saveAsBtn  widget.Clickable
	saveAsPath *field
	exportBtn  widget.Clickable
	op         mutation
	conflict   *conflict
	keepMine   widget.Clickable
	takeTheirs widget.Clickable
	onDisk     bool
	after      pendingMove
	epoch      uint64
	exportErr  error
	exported   string
	creating   bool
}

func newDocument() document {
	return document{text: newField(documentPolicy, false), search: newField(pathPolicy, false), saveAsPath: newField(pathPolicy, false)}
}

func (d *document) reset() {
	d.epoch++
	d.path, d.meta, d.loaded, d.kind, d.loading, d.err = "", nil, loadedFile{}, docNone, false, nil
	d.conflict, d.onDisk, d.after, d.exportErr, d.exported, d.creating = nil, false, moveNone, nil, "", false
	d.img, d.imgSize = paint.ImageOp{}, image.Point{}
	d.op.state, d.op.err = outcomeIdle, nil
	d.text.set("")
}

func (d *document) dirty() bool {
	return d.kind == docText && (d.creating || d.text.text() != string(d.loaded.Data))
}

func readAll(h *Hub, b binding, ws string, req pageRequest, limit uint64) (loadedFile, error) {
	var out loadedFile
	offset := req.offset
	for {
		page, err := loadPage(h, b, ws, pageRequest{path: req.path, expected: req.expected, offset: offset})
		if err != nil {
			return out, err
		}
		if req.expected == "" {
			req.expected, out.Revision, out.Total = page.chunk.Revision, page.chunk.Revision, page.total
		}
		if page.chunk.Revision != out.Revision {
			return out, errReadRevision
		}
		if err := checkPage(page, offset, out.Total, limit, uint64(len(out.Data))); err != nil {
			return out, err
		}
		if page.total > limit {
			return out, nil
		}
		out.Data = append(out.Data, page.data...)
		if page.next >= page.total {
			return out, nil
		}
		offset = page.next
	}
}

func checkPage(page filePage, offset, total, limit, have uint64) error {
	off, err := rpc.ParseDecimal(page.chunk.Offset)
	if err != nil || off != offset {
		return fmt.Errorf("the daemon sent file bytes at the wrong offset")
	}
	if page.total != total {
		return errReadRevision
	}
	got := uint64(len(page.data))
	if got > chunkLimit || have+got > total || have+got > limit {
		return fmt.Errorf("the daemon sent more file bytes than its declared total")
	}
	if page.next <= offset && page.next < total {
		return fmt.Errorf("the daemon stopped sending file bytes before the end")
	}
	return nil
}

type pageRequest struct {
	path     string
	expected string
	offset   uint64
}

type filePage struct {
	chunk readChunk
	total uint64
	next  uint64
	data  []byte
}

func loadPage(h *Hub, b binding, ws string, req pageRequest) (filePage, error) {
	var chunk readChunk
	params := readParams{WorkspaceID: ws, Path: req.path, ExpectedRevision: req.expected, Offset: rpc.FormatDecimal(req.offset), Limit: chunkLimit}
	if err := h.call(b, pageDeadline, methodWorkspaceRead, params, &chunk); err != nil {
		return filePage{}, err
	}
	total, next, err := chunkBounds(chunk)
	if err != nil {
		return filePage{}, err
	}
	data, err := base64.StdEncoding.DecodeString(chunk.DataBase64)
	if err != nil {
		return filePage{}, fmt.Errorf("the daemon sent undecodable file bytes: %w", err)
	}
	return filePage{chunk: chunk, total: total, next: next, data: data}, nil
}

func chunkBounds(c readChunk) (total, next uint64, err error) {
	if total, err = rpc.ParseDecimal(c.Total); err != nil {
		return 0, 0, err
	}
	next, err = rpc.ParseDecimal(c.Next)
	return total, next, err
}

func (d *document) load(f *Files, b binding, p string, m fileMetadata) {
	d.reset()
	d.path, d.meta, d.loading = p, &m, true
	epoch := d.epoch
	limit := uint64(editLimit)
	if isImage(p) {
		limit = imageLimit
	}
	ws, h := f.lease.grant.WorkspaceID, f.hub
	f.work.spawn(func() func() {
		file, err := readAll(h, b, ws, pageRequest{path: p}, limit)
		var img image.Image
		if err == nil && isImage(p) && file.Total <= limit {
			if decoded, derr := decodeImageData(file.Data); derr == nil {
				img = decoded
			}
		}
		return func() {
			if d.epoch != epoch {
				return
			}
			d.loading, d.err = false, err
			if err == nil {
				d.accept(file, limit, img)
			}
		}
	})
}

func isImage(p string) bool {
	switch strings.ToLower(path.Ext(p)) {
	case ".png", ".jpg", ".jpeg", ".gif":
		return true
	}
	return false
}

func (d *document) accept(file loadedFile, limit uint64, img image.Image) {
	d.loaded, d.onDisk = file, false
	switch {
	case file.Total > limit:
		d.kind = docTooLarge
	case isImage(d.path) && img != nil:
		d.kind, d.img, d.imgSize = docImage, paint.NewImageOp(img), img.Bounds().Size()
	case isImage(d.path):
		d.kind = docBinary
	case bytes.IndexByte(file.Data, 0) >= 0:
		d.kind = docBinary
	case !utf8.Valid(file.Data):
		d.kind = docUndecodable
	default:
		d.kind = docText
		d.text.set(string(file.Data))
	}
}

func decodeImageData(data []byte) (image.Image, error) {
	cfg, _, err := image.DecodeConfig(bytes.NewReader(data))
	if err != nil {
		return nil, err
	}
	if cfg.Width <= 0 || cfg.Height <= 0 || cfg.Width > maxImageDimension || cfg.Height > maxImageDimension {
		return nil, errImageTooLarge
	}
	if uint64(cfg.Width)*uint64(cfg.Height) > maxImagePixels {
		return nil, errImageTooLarge
	}
	img, _, err := image.Decode(bytes.NewReader(data))
	return img, err
}

func (d *document) create(f *Files, b binding, p string) {
	if d.dirty() {
		f.moving, f.nextDir, f.nextFile = moveTarget, f.dir, nil
		return
	}
	d.reset()
	d.path, d.kind, d.creating = p, docText, true
	d.save(f, b, p, absent)
}

func (d *document) update(gtx layout.Context, f *Files, b binding) {
	d.text.update(gtx)
	d.search.update(gtx)
	d.saveAsPath.update(gtx)
	off := d.op.running() || !b.live
	d.updateSave(gtx, f, b, off)
	d.updateSaveAs(gtx, f, b, off)
	d.updateFind(gtx)
	d.updateExport(gtx, f, b)
	d.updateConflict(gtx, f, b)
}

func (d *document) updateSave(gtx layout.Context, f *Files, b binding, off bool) {
	if clicked(gtx, &d.saveBtn, off || d.kind != docText || !d.dirty()) {
		d.save(f, b, d.path, cmpOr(d.loaded.Revision, absent))
	}
}

func (d *document) updateSaveAs(gtx layout.Context, f *Files, b binding, off bool) {
	if clicked(gtx, &d.saveAsBtn, off || d.kind != docText || textinput.VisuallyBlank(d.saveAsPath.text())) {
		d.save(f, b, strings.Trim(d.saveAsPath.text(), "/"), absent)
	}
}

func (d *document) updateFind(gtx layout.Context) {
	if clicked(gtx, &d.findNext, d.kind != docText || d.search.text() == "") {
		d.find()
	}
}

func (d *document) updateExport(gtx layout.Context, f *Files, b binding) {
	if clicked(gtx, &d.exportBtn, d.path == "" || d.loading) {
		d.export(f, b)
	}
}

func (d *document) find() {
	e, query := d.text.ensure(), d.search.text()
	if e == nil || query == "" {
		return
	}
	draft := e.Draft()
	if !selectNext(e, draft.Text, query, draft.Selection.End) {
		selectNext(e, draft.Text, query, 0)
	}
}

func selectNext(e *platform.Editor, text, query string, from int) bool {
	for at := from; at <= len(text); {
		i := strings.Index(text[at:], query)
		if i < 0 {
			return false
		}
		if e.Select(textinput.Range{Start: at + i, End: at + i + len(query)}) == nil {
			return true
		}
		at += i + 1
	}
	return false
}

func (d *document) matches() int {
	q := d.search.text()
	if q == "" {
		return 0
	}
	return strings.Count(d.text.text(), q)
}

func (d *document) save(f *Files, b binding, p, expected string) {
	data := []byte(d.text.text())
	commitID := d.op.begin(methodWorkspaceWrite, b, true)
	l := f.lease
	if !f.hub.workspaces.own(l) {
		d.op.state, d.op.err = outcomeFailed, fmt.Errorf("the workspace handle on %s is gone; open the workspace again", f.target.Endpoint)
		return
	}
	epoch := d.epoch
	ws, h := l.grant.WorkspaceID, f.hub
	f.work.spawn(func() func() {
		meta, err := upload(h, b, ws, uploadRequest{path: p, expected: expected, commitID: commitID}, data)
		return func() {
			f.settleOwned(l, &d.op, err)
			if d.epoch != epoch || d.op.id != commitID {
				return
			}
			d.op.settle(err)
			d.saved(f, b, savedResult{path: p, data: data, meta: meta, err: err})
		}
	})
}

type uploadRequest struct {
	path     string
	expected string
	commitID string
}

type savedResult struct {
	path string
	data []byte
	meta fileMetadata
	err  error
}

func upload(h *Hub, b binding, ws string, req uploadRequest, data []byte) (fileMetadata, error) {
	sum := sha256.Sum256(data)
	begin := writeBegin{Stage: "begin", WorkspaceID: ws, Path: req.path, ExpectedRevision: req.expected, Total: strconv.Itoa(len(data)), Digest: hex.EncodeToString(sum[:]), OperationID: rpc.NewOperationID()}
	var begun writeBegun
	if err := h.call(b, decisionDeadline, methodWorkspaceWrite, begin, &begun); err != nil {
		return fileMetadata{}, knownBeforeCommit(err)
	}
	for offset := 0; offset < len(data); offset += chunkLimit {
		end := min(offset+chunkLimit, len(data))
		chunk := writeChunk{Stage: "chunk", UploadID: begun.UploadID, Offset: strconv.Itoa(offset), DataBase64: base64.StdEncoding.EncodeToString(data[offset:end])}
		if err := h.call(b, pageDeadline, methodWorkspaceWrite, chunk, nil); err != nil {
			_ = h.call(b, metadataDeadline, methodWorkspaceWrite, writeCommit{Stage: "abort", UploadID: begun.UploadID}, nil)
			return fileMetadata{}, knownBeforeCommit(err)
		}
	}
	var res metadataResult
	err := h.call(b, decisionDeadline, methodWorkspaceWrite, writeCommit{Stage: "commit", UploadID: begun.UploadID, OperationID: req.commitID}, &res)
	return res.Metadata, err
}

func knownBeforeCommit(err error) error {
	if _, coded := errors.AsType[*rpc.RPCError](err); coded {
		return err
	}
	return &rpc.RPCError{Code: -32010, Message: "the upload stopped before commit; nothing was written: " + err.Error()}
}

func (d *document) saved(f *Files, b binding, done savedResult) {
	if kindOf(done.err) == rpc.ErrorStaleRevision {
		d.op.state, d.op.err = outcomeIdle, nil
		d.openConflict(f, b, done.path)
		return
	}
	if done.err != nil {
		return
	}
	d.path, d.creating, d.conflict, d.onDisk = done.path, false, nil, false
	d.loaded = loadedFile{Revision: done.meta.Revision, Total: uint64(len(done.data)), Data: done.data}
	d.meta = &done.meta
	f.announce(platform.AnnounceCompletion, "Saved "+done.path+" on "+string(f.target.Endpoint))
	f.list(b, f.dir, "")
	if move := d.after; move != moveNone {
		d.after = moveNone
		f.moving, f.pendingFinish = move, true
	}
}

func (d *document) openConflict(f *Files, b binding, p string) {
	d.conflict = &conflict{path: p, loading: true}
	c, epoch := d.conflict, d.epoch
	ws, h := f.lease.grant.WorkspaceID, f.hub
	f.work.spawn(func() func() {
		theirs, err := readAll(h, b, ws, pageRequest{path: p}, editLimit)
		var meta *fileMetadata
		var stat fileMetadata
		if serr := h.call(b, metadataDeadline, methodWorkspaceStat, pathParams{WorkspaceID: ws, Path: p}, &stat); serr == nil {
			meta = &stat
		}
		return func() {
			if d.epoch != epoch || d.conflict != c {
				return
			}
			c.loading, c.err = false, err
			if err == nil || kindOf(err) == rpc.ErrorNotFound {
				c.theirs, c.err, c.meta = &theirs, nil, meta
			}
		}
	})
}

func (d *document) updateConflict(gtx layout.Context, f *Files, b binding) {
	c := d.conflict
	if c == nil {
		return
	}
	if clicked(gtx, &d.keepMine, c.theirs == nil || d.op.running()) {
		d.save(f, b, c.path, cmpOr(c.theirs.Revision, absent))
	}
	if clicked(gtx, &d.takeTheirs, c.theirs == nil || d.op.running()) {
		d.adoptTheirs(f, b, c)
	}
}

func (d *document) adoptTheirs(f *Files, b binding, c *conflict) {
	path, theirs, meta := c.path, *c.theirs, c.meta
	epoch := d.epoch
	d.conflict, d.creating = nil, false
	d.path, d.meta = path, meta
	if !isImage(path) {
		d.accept(theirs, editLimit, nil)
		return
	}
	d.loading = true
	f.work.spawn(func() func() {
		var img image.Image
		if decoded, derr := decodeImageData(theirs.Data); derr == nil {
			img = decoded
		}
		return func() {
			if d.epoch != epoch {
				return
			}
			d.loading = false
			d.accept(theirs, editLimit, img)
		}
	})
}

func (d *document) watched(f *Files, b binding, change workspaceChange) {
	if d.path == "" || path.Dir(d.path) != cleanDir(f.dir) || !touches(change, d.path) {
		return
	}
	epoch, ws := d.epoch, f.lease.grant.WorkspaceID
	f.work.spawn(func() func() {
		var meta fileMetadata
		err := f.hub.call(b, metadataDeadline, methodWorkspaceStat, pathParams{WorkspaceID: ws, Path: d.path}, &meta)
		return func() {
			if d.epoch != epoch || err != nil || meta.Revision == d.loaded.Revision {
				return
			}
			d.meta = &meta
			if d.dirty() {
				d.onDisk = true
				return
			}
			d.load(f, b, d.path, meta)
		}
	})
}

func cleanDir(dir string) string {
	if dir == "" {
		return "."
	}
	return dir
}

func touches(change workspaceChange, p string) bool {
	return change.RescanRequired || slices.ContainsFunc(change.Paths, func(x string) bool { return x == p || x == path.Base(p) })
}

func (d *document) export(f *Files, b binding) {
	p, size := d.path, uint64(0)
	if d.meta != nil {
		size, _ = strconv.ParseUint(d.meta.Size, 10, 64)
	}
	d.exportErr, d.exported = nil, ""
	if size > transferLimit {
		d.exportErr = errTooLarge
		return
	}
	epoch, ws := d.epoch, f.lease.grant.WorkspaceID
	f.work.spawn(func() func() {
		file, err := readAll(f.hub, b, ws, pageRequest{path: p}, transferLimit)
		if err == nil && file.Total > transferLimit {
			err = errTooLarge
		}
		if err == nil {
			err = f.hub.export(path.Base(p), file.Data)
		}
		return func() {
			if d.epoch != epoch {
				return
			}
			d.exportErr = err
			if err == nil {
				d.exported = "Exported " + path.Base(p) + " from " + string(f.target.Endpoint) + "."
			}
		}
	})
}

func (d *document) widgets(env scene, f *Files, b binding) []layout.Widget {
	if d.path == "" {
		return nil
	}
	out := []layout.Widget{sayer(env, toneHeading, f.displayPath(d.path))}
	if d.meta != nil {
		out = append(out, sayer(env, toneDim, fmt.Sprintf("%s · %s · revision %s", sizeText(*d.meta), cmpOr(d.meta.ModifiedAt, "unknown time"), cmpOr(d.loaded.Revision, d.meta.Revision))))
		out = append(out, buttons(env,
			action{click: &f.renameBtn, label: "Rename this file", variant: quiet, off: f.op.running() || d.dirty()},
			action{click: &f.removeBtn, label: "Remove this file", variant: destructive, off: f.op.running() || d.dirty()}))
	}
	switch {
	case d.loading:
		return append(out, sayer(env, toneDim, "Loading…"))
	case d.err != nil:
		return append(out, sayer(env, toneFailed, "The file did not load: "+explain(d.err)))
	}
	out = append(out, d.content(env, b)...)
	if d.exportErr != nil {
		out = append(out, sayer(env, toneFailed, "Export failed: "+d.exportErr.Error()))
	}
	if d.exported != "" {
		out = append(out, sayer(env, toneAdded, d.exported))
	}
	return out
}

func (d *document) content(env scene, b binding) []layout.Widget {
	export := buttons(env, action{click: &d.exportBtn, label: "Export file", variant: secondary})
	switch d.kind {
	case docTooLarge:
		return []layout.Widget{sayer(env, toneBody, fmt.Sprintf("This file is %s; the viewer loads at most %s. Export it to open it elsewhere.", byteSize(d.loaded.Total), byteSize(editLimit))), export}
	case docBinary:
		return []layout.Widget{sayer(env, toneBody, "Binary file; it is not shown as text."), export}
	case docUndecodable:
		return []layout.Widget{sayer(env, toneWarning, "This file is not valid UTF-8, so editing is unavailable; its bytes stay exactly as they are. Export it to work with it elsewhere."), export}
	case docImage:
		return []layout.Widget{d.imageView(env), sayer(env, toneDim, fmt.Sprintf("Image %d × %d pixels", d.imgSize.X, d.imgSize.Y)), export}
	case docText:
		return d.editorWidgets(env, b, export)
	case docNone:
	}
	return nil
}

func (d *document) imageView(env scene) layout.Widget {
	return func(gtx layout.Context) layout.Dimensions {
		gtx.Constraints.Max.Y = min(gtx.Constraints.Max.Y, gtx.Dp(unit.Dp(480)))
		return widget.Image{Src: d.img, Fit: widget.ScaleDown, Position: layout.NW}.Layout(gtx)
	}
}

func (d *document) editorWidgets(env scene, b binding, export layout.Widget) []layout.Widget {
	readOnly := !b.live || d.meta != nil && !d.meta.Writable && !d.creating
	out := d.editorBanners(env, b, readOnly)
	out = append(out, d.conflictWidgets(env)...)
	out = append(out,
		func(gtx layout.Context) layout.Dimensions {
			return d.text.layout(gtx, env, "File text", readOnly || d.op.running())
		},
		func(gtx layout.Context) layout.Dimensions { return d.search.layout(gtx, env, "Find in file", false) },
		buttons(env, action{click: &d.findNext, label: fmt.Sprintf("Find next (%d matches)", d.matches()), variant: quiet, off: d.search.text() == ""}),
		buttons(env,
			action{click: &d.saveBtn, label: "Save", variant: pointed, off: readOnly || d.op.running() || !d.dirty()},
			action{click: &d.saveAsBtn, label: "Save as", variant: secondary, off: !b.live || d.op.running() || textinput.VisuallyBlank(d.saveAsPath.text())}),
		func(gtx layout.Context) layout.Dimensions {
			return d.saveAsPath.layout(gtx, env, "Save as path", d.op.running())
		},
		d.op.line(env),
		export)
	return out
}

func (d *document) editorBanners(env scene, b binding, readOnly bool) []layout.Widget {
	out := []layout.Widget{}
	switch {
	case !b.live:
		out = append(out, sayer(env, toneWarning, "Disconnected: editing is paused until this endpoint reconnects."))
	case readOnly:
		out = append(out, sayer(env, toneWarning, "Read-only: the runner reports this file as not writable."))
	}
	if d.onDisk {
		out = append(out, sayer(env, toneWarning, "This file changed on its endpoint since you loaded it; saving asks you to resolve the difference."))
	}
	return out
}

func (d *document) conflictWidgets(env scene) []layout.Widget {
	c := d.conflict
	if c == nil {
		return nil
	}
	switch {
	case c.loading:
		return []layout.Widget{sayer(env, toneWarning, "The file changed since you loaded it. Loading the current version…")}
	case c.err != nil:
		return []layout.Widget{sayer(env, toneFailed, "The file changed since you loaded it, and its current version did not load: "+explain(c.err))}
	}
	theirs := "The file was removed on the endpoint."
	if c.theirs.Revision != "" {
		theirs = "Current version on the endpoint:\n" + previewText(c.theirs.Data)
	}
	return []layout.Widget{
		sayer(env, toneWarning, "The file changed since you loaded it. Choose which version to keep; your text stays in the editor until you decide."),
		sayer(env, toneCode, theirs),
		buttons(env,
			action{click: &d.keepMine, label: "Keep my version", variant: pointed},
			action{click: &d.takeTheirs, label: "Use their version", variant: secondary}),
	}
}

func previewText(data []byte) string {
	const limit = 4096
	if !utf8.Valid(data) || bytes.IndexByte(data, 0) >= 0 {
		return fmt.Sprintf("(%s of binary data)", byteSize(uint64(len(data))))
	}
	if len(data) <= limit {
		return string(data)
	}
	cut := limit
	for !utf8.RuneStart(data[cut]) {
		cut--
	}
	return string(data[:cut]) + fmt.Sprintf("\n… %s more", byteSize(uint64(len(data))-uint64(cut)))
}
