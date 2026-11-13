package agent

import (
	"github.com/gosuda/dalgona/internal/textdiff"
	"github.com/gosuda/dalgona/internal/tool"
)

func (w *worker) approvalFiles(files []tool.FileChange) ([]ApprovalFile, error) {
	out := make([]ApprovalFile, 0, len(files))
	for _, file := range files {
		pre, err := w.t.host.maskBytes(file.Pre)
		if err != nil {
			return nil, err
		}
		post, err := w.t.host.maskBytes(file.Post)
		if err != nil {
			return nil, err
		}
		item := ApprovalFile{Path: file.Path, Added: file.PreSHA == "", Deleted: file.PostSHA == ""}
		if len(pre) > textdiff.Limit || len(post) > textdiff.Limit {
			item.TooLarge = true
		} else {
			item.Pre, item.Post = pre, post
		}
		out = append(out, item)
	}
	return out, nil
}

func (h *Host) maskBytes(data []byte) ([]byte, error) {
	if data == nil {
		return nil, nil
	}
	text, err := h.mask(string(data))
	if err != nil {
		return nil, err
	}
	return []byte(text), nil
}
