package rpc

import (
	"context"
	"encoding/base64"
	"encoding/json/jsontext"
	"errors"
	"io"
	"io/fs"
	"os"
	"path/filepath"
)

const fsFileLimit = 16 << 20

type fsParams struct {
	path, sourcePath, destinationPath, dataBase64 string
	recursive, force                              *bool
}

func decodeFsParams(params jsontext.Value) (fsParams, error) {
	var p fsParams
	err := decodeParams(params, func(name string, d *jsontext.Decoder) error {
		var err error
		switch name {
		case "path":
			p.path, err = readString(d)
		case "sourcePath":
			p.sourcePath, err = readString(d)
		case "destinationPath":
			p.destinationPath, err = readString(d)
		case "dataBase64":
			p.dataBase64, err = readString(d)
		case "recursive":
			p.recursive, err = readOptBool(d)
		case "force":
			p.force, err = readOptBool(d)
		default:
			err = d.SkipValue()
		}
		return err
	})
	return p, err
}

func readOptBool(d *jsontext.Decoder) (*bool, error) {
	if d.PeekKind() == jsontext.KindNull {
		_, err := d.ReadToken()
		return nil, err
	}
	v, err := readBool(d)
	if err != nil {
		return nil, err
	}
	return &v, nil
}

func (p fsParams) recursiveByDefault() bool { return p.recursive == nil || *p.recursive }

func (p fsParams) recursiveWhenSet() bool { return p.recursive != nil && *p.recursive }

func (p fsParams) forceByDefault() bool { return p.force == nil || *p.force }

func absPath(p string) (string, error) {
	if !filepath.IsAbs(p) {
		return "", invalidParams("path must be absolute")
	}
	return filepath.Clean(p), nil
}

func fsError(path string, err error) error {
	if errors.Is(err, fs.ErrNotExist) {
		return invalidParams("no such file: " + path)
	}
	return internalError(err)
}

func (s *server) fsReadFile(_ context.Context, params jsontext.Value) (jsontext.Value, error) {
	p, err := decodeFsParams(params)
	if err != nil {
		return nil, err
	}
	path, err := absPath(p.path)
	if err != nil {
		return nil, err
	}
	info, err := os.Stat(path)
	if err != nil {
		return nil, fsError(path, err)
	}
	if !info.Mode().IsRegular() {
		return nil, invalidParams("not a regular file: " + path)
	}
	if info.Size() > fsFileLimit {
		return nil, invalidParams("file larger than 16 MiB")
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fsError(path, err)
	}
	o := newOut()
	o.open()
	o.str("dataBase64", base64.StdEncoding.EncodeToString(data))
	o.close()
	return o.bytes()
}

func (s *server) fsWriteFile(_ context.Context, params jsontext.Value) (jsontext.Value, error) {
	p, err := decodeFsParams(params)
	if err != nil {
		return nil, err
	}
	path, err := absPath(p.path)
	if err != nil {
		return nil, err
	}
	data, err := base64.StdEncoding.DecodeString(p.dataBase64)
	if err != nil {
		return nil, invalidParams("dataBase64 is not valid base64")
	}
	if len(data) > fsFileLimit {
		return nil, invalidParams("file larger than 16 MiB")
	}
	if err := writeFileAtomic(path, data); err != nil {
		return nil, fsError(path, err)
	}
	return emptyObject(), nil
}

func writeFileAtomic(path string, data []byte) error {
	mode := fs.FileMode(0o644)
	if info, err := os.Stat(path); err == nil {
		mode = info.Mode().Perm()
	}
	tmp, err := os.CreateTemp(filepath.Dir(path), ".dalgona-write-*")
	if err != nil {
		return err
	}
	name := tmp.Name()
	if err := writeTemp(tmp, data, mode); err != nil {
		_ = os.Remove(name)
		return err
	}
	if err := os.Rename(name, path); err != nil {
		_ = os.Remove(name)
		return err
	}
	return nil
}

func writeTemp(f *os.File, data []byte, mode fs.FileMode) error {
	_, err := f.Write(data)
	if err == nil {
		err = f.Chmod(mode)
	}
	if closeErr := f.Close(); err == nil {
		err = closeErr
	}
	return err
}

func (s *server) fsCreateDirectory(_ context.Context, params jsontext.Value) (jsontext.Value, error) {
	p, err := decodeFsParams(params)
	if err != nil {
		return nil, err
	}
	path, err := absPath(p.path)
	if err != nil {
		return nil, err
	}
	if p.recursiveByDefault() {
		err = os.MkdirAll(path, 0o750)
	} else {
		err = os.Mkdir(path, 0o750)
	}
	if err != nil {
		return nil, internalError(err)
	}
	return emptyObject(), nil
}

func (s *server) fsGetMetadata(_ context.Context, params jsontext.Value) (jsontext.Value, error) {
	p, err := decodeFsParams(params)
	if err != nil {
		return nil, err
	}
	path, err := absPath(p.path)
	if err != nil {
		return nil, err
	}
	link, err := os.Lstat(path)
	if err != nil {
		return nil, fsError(path, err)
	}
	info, err := os.Stat(path)
	if errors.Is(err, fs.ErrNotExist) {
		info = link
	} else if err != nil {
		return nil, internalError(err)
	}
	o := newOut()
	o.open()
	o.boolean("isDirectory", info.IsDir())
	o.boolean("isFile", info.Mode().IsRegular())
	o.boolean("isSymlink", link.Mode()&fs.ModeSymlink != 0)
	o.int("createdAtMs", 0)
	o.int("modifiedAtMs", info.ModTime().UnixMilli())
	o.close()
	return o.bytes()
}

func (s *server) fsReadDirectory(_ context.Context, params jsontext.Value) (jsontext.Value, error) {
	p, err := decodeFsParams(params)
	if err != nil {
		return nil, err
	}
	path, err := absPath(p.path)
	if err != nil {
		return nil, err
	}
	info, err := os.Stat(path)
	if err != nil {
		return nil, fsError(path, err)
	}
	if !info.IsDir() {
		return nil, invalidParams("not a directory: " + path)
	}
	entries, err := os.ReadDir(path)
	if err != nil {
		return nil, internalError(err)
	}
	o := newOut()
	o.open()
	o.key("entries")
	o.openArray()
	for _, entry := range entries {
		isDir, isFile := false, false
		if target, statErr := os.Stat(filepath.Join(path, entry.Name())); statErr == nil {
			isDir, isFile = target.IsDir(), target.Mode().IsRegular()
		}
		o.open()
		o.str("fileName", entry.Name())
		o.boolean("isDirectory", isDir)
		o.boolean("isFile", isFile)
		o.close()
	}
	o.closeArray()
	o.close()
	return o.bytes()
}

func (s *server) fsRemove(_ context.Context, params jsontext.Value) (jsontext.Value, error) {
	p, err := decodeFsParams(params)
	if err != nil {
		return nil, err
	}
	path, err := absPath(p.path)
	if err != nil {
		return nil, err
	}
	if _, statErr := os.Lstat(path); statErr != nil {
		if errors.Is(statErr, fs.ErrNotExist) && p.forceByDefault() {
			return emptyObject(), nil
		}
		return nil, fsError(path, statErr)
	}
	if p.recursiveByDefault() {
		err = os.RemoveAll(path)
	} else {
		err = os.Remove(path)
	}
	if err != nil {
		return nil, internalError(err)
	}
	return emptyObject(), nil
}

func (s *server) fsCopy(_ context.Context, params jsontext.Value) (jsontext.Value, error) {
	p, err := decodeFsParams(params)
	if err != nil {
		return nil, err
	}
	src, err := absPath(p.sourcePath)
	if err != nil {
		return nil, err
	}
	dst, err := absPath(p.destinationPath)
	if err != nil {
		return nil, err
	}
	info, err := os.Stat(src)
	if err != nil {
		return nil, fsError(src, err)
	}
	if info.IsDir() {
		if !p.recursiveWhenSet() {
			return nil, invalidParams("copying a directory needs recursive")
		}
		err = os.CopyFS(dst, os.DirFS(src))
	} else {
		err = copyFile(src, dst, info.Mode().Perm())
	}
	if err != nil {
		return nil, internalError(err)
	}
	return emptyObject(), nil
}

func copyFile(src, dst string, perm fs.FileMode) error {
	in, err := os.Open(src)
	if err != nil {
		return err
	}
	defer func() { _ = in.Close() }()
	out, err := os.OpenFile(dst, os.O_WRONLY|os.O_CREATE|os.O_EXCL, perm)
	if err != nil {
		return err
	}
	_, err = io.Copy(out, in)
	if closeErr := out.Close(); err == nil {
		err = closeErr
	}
	return err
}
