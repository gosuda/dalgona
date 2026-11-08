package patch

import "github.com/gosuda/dalgona/internal/tool"

const replaceDescription = "Change files. Each entry of changes has a path and one action. old and new: replace old with new; copy old from read or search output without the <n>: prefixes. old must occur once; when it occurs more than once, add line, the line where your copy starts, or set all to true with the file's tag to replace every occurrence. Trailing spaces and curly quotes or dashes may differ; indentation must match. tag and new: replace the whole file; the tag is on the last line of a read that showed the whole file. create: make a new file. delete: true removes the file. rename: move the file. All changes to one file match its content from before this call and must not overlap. Line endings and a byte order mark are kept. The result shows each changed region as <n>:<text> lines."

const anchorDescription = `Change files with one payload. *** File: path opens a file; a bare *** File: continues it; repeat for more files; every section applies atomically. Each edit is *** Find and one action. A Find body copies current text exactly, without read prefixes. *** Find @40 adds the line where the text starts; use it only when the text occurs more than once. *** Find 40-88 addresses a span: quote the file's current line 40 first and line 88 last, or quote every line of the span; you must have been shown every line of the span. *** Find all changes every match and needs the file tag: *** File: path #TAG, copied from a read that showed the whole file. Actions: *** Replace with the final text, empty deletes; *** Insert Before and *** Insert After add lines and keep the match. *** Replace File rewrites the whole file and needs the tag. *** New File: path plus a body creates; *** Delete File: path removes; *** Move: old -> new renames. Every edit addresses the file as it was before this payload, and edits to one file must not overlap. Trailing spaces and curly quotes or dashes may differ; indentation must match. The result shows each changed region with line numbers, so your next edit can copy from it. A failed payload changes nothing: fix it and resend the whole payload.`

const hashlineDescription = "Hashline patches files. Each file: `[PATH#TAG]`, `TAG` required 4-hex snapshot from latest `read`/`search`. New file: `[PATH#NEW]` then `PUT >$:` with `+` rows. Numbers: original `LINE:TEXT`, never hunk-shifted.\n\n<ops>\n`PUT N.=M:` replace inclusive N–M with `+` body (`N.=N` for one line).\n`PUT <N:`/`PUT >N:` insert before/after N (`<1` head, `>$` tail).\n`CUT N.=M` delete.\n`REM` delete file; `MV DEST` rename after prior edits.\n</ops>\n\n<rules>\n- `:` ops only: body rows `+TEXT` verbatim incl. indent; lone `+` blank. Literal `- item`/`+ item` → `+- item`/`++ item`. NEVER `-`/bare context. Body length independent of range; delete with CUT, not empty PUT.\n- Touch displayed changed lines only; `…`, `..`, collapsed `N-M:` and out-of-window lines UNSEEN. Re-read first. Tight ranges: split nonadjacent changes; NEVER include keepers or start/end mid-expression/block. Pure addition uses gap PUT.\n- Markdown heading blocks run through deeper headings until next same/higher.\n- NEVER restyle unrelated code. After EVERY edit tag/numbers change: use edit response or fresh `read`; stale tag/surprise → STOP, re-read.\n</rules>\n\n<example>\n[greet.py#A1B2]\nPUT 1.=2:\n+@cache\n+def greet(name):\n+    print(name)\n[PLAN.md#3C4D]\nPUT >2:\n+- task\n</example>"

const lightDescription = "Edit files from a read/search snapshot. Copy its [path@reference] header.\nPUT N: or PUT N-M: replaces original lines using + body rows. CUT N or\nCUT N-M removes them. PUT <N:, PUT >N:, and PUT >$: insert in original\nsnapshot gaps.\n[PATH#NEW] plus PUT >$: creates a file. REM explicitly deletes; MV DEST\nrenames after edits. REM/MV require the unchanged complete source file.\nAll operations use the referenced before-image. Light permits replacement\nof lines not fully displayed, but rejects changed targets, missing history,\ninvalid paths and stale approval. Known unrelated edits can move an untouched\ntarget; the host maps them. Unknown edits require a new read. Never invent\na reference or choose a target by similar text. Empty PUT is invalid; use\nCUT. A valid no-op is not an error or proof of task completion. Pre-commit\nrefusal writes no target. Inspect commit I/O outcomes; never retry blindly."

const enhancedDescription = "Edit files from a read/search snapshot. Copy its [path@reference] header.\nPUT N: or PUT N-M: replaces original lines using + body rows. CUT removes\noriginal lines. PUT <N:, PUT >N:, and PUT >$: insert in source-snapshot\ngaps. Every existing cell\nchanged by replacement or CUT must have been fully shown to this caller.\nFolded/truncated rows do not count. A missing-context reply can supply rows\nfor a later request, never this one. The host maps intact targets through\nknown edits. Equal text elsewhere is not the target; missing history needs\na fresh read. Known changed targets stay invalid even if old text returns.\n[PATH#NEW] creates. REM explicitly deletes under unchanged full-file\nreference and approval; REM is exempt from the every-line display rule.\nMV DEST renames after edits. Do not mix references or overlap operations.\nA valid no-op is not an error or proof of task completion. Never downgrade\nto Light to bypass refusal. Never blind-retry an uncertain commit."

const applyPatchDescription = "The `patch` tool can be used to edit files. This is a FREEFORM tool, so do not wrap the patch in JSON."

func description(style Style) string {
	switch style {
	case Anchor:
		return anchorDescription
	case Replace:
		return replaceDescription
	case Hashline:
		return hashlineDescription
	case HashlineLight:
		return lightDescription
	case HashlineEnhanced:
		return enhancedDescription
	case ApplyPatch:
		return applyPatchDescription
	default:
		return ""
	}
}

func schema(style Style) []byte {
	if style == Replace {
		return []byte(`{"type":"object","properties":{"changes":{"type":"array","minItems":1,"maxItems":64,"items":{"type":"object","properties":{"path":{"type":"string"},"old":{"type":"string"},"line":{"type":"integer","minimum":1},"all":{"type":"boolean"},"new":{"type":"string"},"tag":{"type":"string"},"create":{"type":"string"},"delete":{"type":"boolean"},"rename":{"type":"string"}},"required":["path"],"additionalProperties":false}}},"required":["changes"],"additionalProperties":false}`)
	}
	return []byte(`{"type":"object","properties":{"input":{"type":"string"}},"required":["input"],"additionalProperties":false}`)
}

func specFor(style Style) tool.Spec {
	name := "patch"
	freeform := style == ApplyPatch
	if style == ApplyPatch {
		name = "apply_patch"
	}
	return tool.Spec{Name: name, Description: description(style), Schema: schema(style), Freeform: freeform, Keep: tool.KeepHead}
}
