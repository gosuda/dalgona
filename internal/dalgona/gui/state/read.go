package state

import (
	"context"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"errors"
	"fmt"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const readLimit = 16 << 20

var errContentMismatch = errors.New("referenced content does not match its summary")

type fetchChunk func(ctx context.Context, offset string) (rpc.ContentChunk, error)

func readAll(ctx context.Context, fetch fetchChunk, summary rpc.ContentSummary) ([]byte, error) {
	total, err := rpc.ParseDecimal(summary.Total)
	if err != nil {
		return nil, fmt.Errorf("%w: total %w", errContentMismatch, err)
	}
	if total > readLimit {
		return nil, rpc.NativeKind(rpc.ErrorLimitExceeded)
	}
	data := make([]byte, 0, total)
	for {
		chunk, err := fetch(ctx, rpc.FormatDecimal(uint64(len(data))))
		if err != nil {
			return nil, err
		}
		part, err := chunkBytes(chunk, uint64(len(data)), total, summary)
		if err != nil {
			return nil, err
		}
		data = append(data, part...)
		if uint64(len(data)) == total {
			return data, nil
		}
		if len(part) == 0 {
			return nil, fmt.Errorf("%w: no progress at offset %d", errContentMismatch, len(data))
		}
	}
}

func chunkBytes(chunk rpc.ContentChunk, offset, total uint64, summary rpc.ContentSummary) ([]byte, error) {
	if chunk.Total != summary.Total || chunk.Digest != summary.Digest || chunk.Offset != rpc.FormatDecimal(offset) {
		return nil, fmt.Errorf("%w: chunk at %s of %s", errContentMismatch, chunk.Offset, chunk.Total)
	}
	part, err := base64.StdEncoding.DecodeString(chunk.DataBase64)
	if err != nil {
		return nil, fmt.Errorf("%w: %w", errContentMismatch, err)
	}
	next, err := rpc.ParseDecimal(chunk.Next)
	if err != nil || next != offset+uint64(len(part)) || next > total {
		return nil, fmt.Errorf("%w: chunk ends at %s", errContentMismatch, chunk.Next)
	}
	return part, nil
}

func checkContent(summary rpc.ContentSummary, data []byte, err error) ([]byte, error) {
	if err != nil {
		return nil, err
	}
	sum := sha256.Sum256(data)
	if summary.Total != rpc.FormatDecimal(uint64(len(data))) || summary.Digest != hex.EncodeToString(sum[:]) {
		return nil, errContentMismatch
	}
	return data, nil
}
