package runner

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/base64"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"runtime/debug"
	"strconv"
	"strings"
	"sync"
	"time"

	"golang.org/x/sync/errgroup"
)

const (
	uploadFlight   = 16
	chmodBatchSize = 64
	pollInterval   = 20 * time.Millisecond
	b64LineBytes   = 57
	maxPathBuf     = 8 << 20
	workspaceRoot  = "/workspace"
	baselinePath   = "/workspace/.dalgona-baseline"
	uploadScript   = `mkdir -p "$(dirname "$1")" && tmp="$1.tmp.$$" && trap 'rm -f "$tmp"' EXIT && head -c "$3" | base64 -d > "$tmp" && chmod "$2" "$tmp" && mv "$tmp" "$1"`
	chmodScript    = `chmod +x "$@"`
	treeScript     = `cd /workspace && git init -q && git add -A && git write-tree`
	probeScript    = `uname -m; ~/.dalgona/bin/dalgona --version 2>&1`
)

// Stream is a command pipe. Wait must return after Close: waitStream
// closes the stream on cancel and again after the grace period, and the
// worker goroutine exits only when Wait returns.
type Stream interface {
	io.ReadWriteCloser
	Wait() (exitCode int, err error)
}
type RunFunc func(ctx context.Context, argv []string) (Stream, error)

type BootstrapFunc func(ctx context.Context, boxID string, run RunFunc) error

var _ BootstrapFunc = Bootstrap

type uploadJob struct {
	local  string
	remote string
	perm   fs.FileMode
}

type streamRead struct {
	data []byte
	err  error
}

type waitResult struct {
	code int
	err  error
}

func Bootstrap(ctx context.Context, boxID string, run RunFunc) error {
	out, code, err := runStream(ctx, run, []string{"sh", "-c", probeScript}, nil)
	if err != nil {
		return fmt.Errorf("runner: box %s: %w", boxID, err)
	}
	arch, remote, err := parseProbe(boxID, out)
	if err != nil {
		return fmt.Errorf("runner: %w", err)
	}
	if remote == localVersion() {
		return probeExit(boxID, code)
	}
	if err := requireBase64(ctx, run, boxID); err != nil {
		return err
	}
	return uploadDalgona(ctx, run, arch)
}

func probeExit(boxID string, code int) error {
	if code == 0 {
		return nil
	}
	return fmt.Errorf("runner: box %s: version probe exited %d", boxID, code)
}

func localVersion() string {
	info, ok := debug.ReadBuildInfo()
	if !ok || info == nil || info.Main.Version == "" {
		return "(devel)"
	}
	return info.Main.Version
}

func parseProbe(boxID string, out []byte) (string, string, error) {
	text := strings.TrimSpace(string(out))
	if text == "" {
		return "", "", fmt.Errorf("box %s: empty version probe", boxID)
	}
	line, rest, _ := strings.Cut(text, "\n")
	arch, err := mapArch(strings.TrimSpace(line))
	if err != nil {
		return "", "", fmt.Errorf("box %s: %w", boxID, err)
	}
	return arch, probeVersion(rest), nil
}

func probeVersion(text string) string {
	fields := strings.Fields(text)
	if len(fields) == 0 {
		return ""
	}
	return fields[len(fields)-1]
}

func mapArch(machine string) (string, error) {
	switch machine {
	case "x86_64", "amd64":
		return "amd64", nil
	case "aarch64", "arm64":
		return "arm64", nil
	default:
		return "", fmt.Errorf("unknown machine %s", machine)
	}
}

func requireBase64(ctx context.Context, run RunFunc, boxID string) error {
	_, code, err := runStream(ctx, run, []string{"sh", "-c", "command -v base64"}, nil)
	if err != nil {
		return fmt.Errorf("runner: box %s: %w", boxID, err)
	}
	if code != 0 {
		return fmt.Errorf("runner: box %s has no base64; use an image with coreutils", boxID)
	}
	return nil
}

func uploadDalgona(ctx context.Context, run RunFunc, arch string) error {
	src, err := linuxBinary(arch)
	if err != nil {
		return err
	}
	dest, err := remoteHomeBin(ctx, run)
	if err != nil {
		return err
	}
	return sendBinary(ctx, run, src, dest)
}

func sendBinary(ctx context.Context, run RunFunc, src, dest string) error {
	file, err := os.Open(src)
	if err != nil {
		return fmt.Errorf("runner: open %s: %w", src, err)
	}
	defer func() { _ = file.Close() }()
	info, err := file.Stat()
	if err != nil {
		return fmt.Errorf("runner: stat %s: %w", src, err)
	}
	pr, pw := io.Pipe()
	encErr := make(chan error, 1)
	go func() {
		err := writeBase64(ctx, pw, io.LimitReader(file, info.Size()))
		_ = pw.CloseWithError(err)
		encErr <- err
	}()
	argv := []string{"sh", "-c", uploadScript, "_", dest, "0755", strconv.FormatInt(base64Length(info.Size()), 10)}
	_, code, runErr := runStream(ctx, run, argv, pr)
	_ = pr.Close()
	writeErr := <-encErr
	if runErr != nil {
		return runErr
	}
	if writeErr != nil {
		return fmt.Errorf("runner: encode binary: %w", writeErr)
	}
	if code != 0 {
		return fmt.Errorf("runner: upload dalgona exited %d", code)
	}
	return nil
}

func remoteHomeBin(ctx context.Context, run RunFunc) (string, error) {
	out, code, err := runStream(ctx, run, []string{"sh", "-c", `printf %s "$HOME"`}, nil)
	if err != nil {
		return "", err
	}
	if code != 0 {
		return "", fmt.Errorf("runner: home probe exited %d", code)
	}
	home := strings.TrimSpace(string(out))
	if home == "" {
		return "", errors.New("runner: remote HOME is empty")
	}
	return home + "/.dalgona/bin/dalgona", nil
}

func linuxBinary(arch string) (string, error) {
	exe, err := os.Executable()
	if err != nil {
		return "", fmt.Errorf("runner: executable: %w", err)
	}
	if runtime.GOOS == "linux" && runtime.GOARCH == arch {
		return exe, nil
	}
	sibling := filepath.Join(filepath.Dir(exe), "dalgona-linux-"+arch)
	info, err := os.Stat(sibling)
	if err != nil || !info.Mode().IsRegular() {
		return "", missingBinary(arch, exe, sibling)
	}
	return sibling, nil
}

func missingBinary(arch, exe, sibling string) error {
	return fmt.Errorf("runner: no linux/%s dalgona binary next to %s; build it with GOOS=linux GOARCH=%s CGO_ENABLED=0 go build -o %s ./cmd/dalgona", arch, exe, arch, sibling)
}

func base64Length(size int64) int64 {
	lines, rest := size/b64LineBytes, size%b64LineBytes
	length := lines * int64(base64.StdEncoding.EncodedLen(b64LineBytes)+1)
	if rest > 0 {
		length += int64(base64.StdEncoding.EncodedLen(int(rest)) + 1)
	}
	return length
}

func writeBase64(ctx context.Context, dst io.Writer, src io.Reader) error {
	carry := make([]byte, 0, b64LineBytes)
	buf := make([]byte, b64LineBytes*32)
	for {
		if err := ctx.Err(); err != nil {
			return err
		}
		n, err := src.Read(buf)
		carry = append(carry, buf[:n]...)
		if werr := flushB64Partial(&carry, dst); werr != nil {
			return werr
		}
		if errors.Is(err, io.EOF) {
			return flushB64Final(&carry, dst)
		}
		if err != nil {
			return err
		}
	}
}

func flushB64Partial(carry *[]byte, dst io.Writer) error {
	data := *carry
	off := 0
	for len(data)-off >= b64LineBytes {
		if err := writeB64Line(dst, data[off:off+b64LineBytes]); err != nil {
			return err
		}
		off += b64LineBytes
	}
	rest := len(data) - off
	copy(data, data[off:])
	*carry = data[:rest]
	return nil
}

func flushB64Final(carry *[]byte, dst io.Writer) error {
	if len(*carry) == 0 {
		return nil
	}
	err := writeB64Line(dst, *carry)
	*carry = (*carry)[:0]
	return err
}

func writeB64Line(dst io.Writer, chunk []byte) error {
	enc := make([]byte, base64.StdEncoding.EncodedLen(len(chunk)))
	base64.StdEncoding.Encode(enc, chunk)
	if err := writeFull(dst, enc); err != nil {
		return err
	}
	return writeFull(dst, []byte{'\n'})
}

func runStream(ctx context.Context, run RunFunc, argv []string, stdin io.Reader) ([]byte, int, error) {
	st, err := run(ctx, argv)
	if err != nil {
		return nil, 0, fmt.Errorf("runner: start command: %w", err)
	}
	return finishStream(ctx, st, stdin)
}

func finishStream(ctx context.Context, st Stream, stdin io.Reader) ([]byte, int, error) {
	defer func() { _ = st.Close() }()
	stopClose := context.AfterFunc(ctx, func() { _ = st.Close() })
	defer stopClose()
	outCh := make(chan streamRead, 1)
	go readStream(st, outCh)
	werr := writeStdin(st, stdin)
	if werr != nil {
		_ = st.Close()
	}
	res := <-outCh
	code, waitErr := waitStream(ctx, st)
	if werr != nil {
		return nil, 0, werr
	}
	return acceptRead(res, code, waitErr)
}

func readStream(st Stream, outCh chan<- streamRead) {
	var buf bytes.Buffer
	_, err := io.Copy(&buf, st)
	outCh <- streamRead{data: buf.Bytes(), err: err}
}

func writeStdin(st Stream, stdin io.Reader) error {
	if stdin == nil {
		return nil
	}
	if _, err := io.Copy(st, stdin); err != nil {
		return fmt.Errorf("runner: write command input: %w", err)
	}
	if c, ok := st.(interface{ CloseWrite() error }); ok {
		if err := c.CloseWrite(); err != nil {
			return fmt.Errorf("runner: close command input: %w", err)
		}
	}
	return nil
}

var waitCancelGrace = 5 * time.Second

func waitStream(ctx context.Context, st Stream) (int, error) {
	ch := make(chan waitResult, 1)
	go func() {
		code, err := st.Wait()
		ch <- waitResult{code: code, err: err}
	}()
	select {
	case <-ctx.Done():
		_ = st.Close()
		select {
		case res := <-ch:
			if res.err != nil {
				return res.code, fmt.Errorf("runner: wait for command: %w", ctx.Err())
			}
			return res.code, nil
		case <-time.After(waitCancelGrace):
			_ = st.Close()
			return 0, fmt.Errorf("runner: wait for command: %w", ctx.Err())
		}
	case res := <-ch:
		if res.err != nil {
			if ctx.Err() != nil {
				return res.code, fmt.Errorf("runner: wait for command: %w", ctx.Err())
			}
			return res.code, fmt.Errorf("runner: wait for command: %w", res.err)
		}
		return res.code, nil
	}
}

func acceptRead(res streamRead, code int, waitErr error) ([]byte, int, error) {
	if waitErr != nil {
		return res.data, code, waitErr
	}
	if res.err != nil && !errors.Is(res.err, io.EOF) {
		return res.data, code, fmt.Errorf("runner: read command output: %w", res.err)
	}
	return res.data, code, nil
}

func Upload(ctx context.Context, r Runner, cwd string) (string, error) {
	execPaths, err := uploadFiles(ctx, r, cwd)
	if err != nil {
		return "", err
	}
	if err := chmodExec(ctx, r, execPaths); err != nil {
		return "", err
	}
	return writeBaseline(ctx, r)
}

func uploadFiles(ctx context.Context, r Runner, cwd string) ([]string, error) {
	var mu sync.Mutex
	var execPaths []string
	g, gctx := errgroup.WithContext(ctx)
	g.SetLimit(uploadFlight)
	err := enumerate(gctx, cwd, func(job uploadJob) error {
		if err := gctx.Err(); err != nil {
			return fmt.Errorf("runner: list files: %w", err)
		}
		g.Go(func() error {
			return putUpload(gctx, r, job, &mu, &execPaths)
		})
		return nil
	})
	waitErr := g.Wait()
	if err != nil {
		return nil, err
	}
	return execPaths, waitErr
}

func putUpload(ctx context.Context, r Runner, job uploadJob, mu *sync.Mutex, execPaths *[]string) error {
	if err := putFile(ctx, r, job); err != nil {
		return err
	}
	if job.perm != 0o755 {
		return nil
	}
	mu.Lock()
	*execPaths = append(*execPaths, job.remote)
	mu.Unlock()
	return nil
}

func putFile(ctx context.Context, r Runner, job uploadJob) error {
	file, err := os.Open(job.local)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return fmt.Errorf("runner: open %s: %w", job.local, err)
	}
	defer func() { _ = file.Close() }()
	data, err := io.ReadAll(file)
	if err != nil {
		return fmt.Errorf("runner: read %s: %w", job.local, err)
	}
	if err := r.WriteFile(ctx, job.remote, data, job.perm); err != nil {
		return fmt.Errorf("runner: upload %s: %w", job.remote, err)
	}
	return nil
}

func enumerate(ctx context.Context, cwd string, emit func(uploadJob) error) error {
	inside, err := isWorkTree(ctx, cwd)
	if err != nil {
		return err
	}
	if inside {
		return enumGit(ctx, cwd, emit)
	}
	return enumWalk(cwd, emit)
}

func isWorkTree(ctx context.Context, cwd string) (bool, error) {
	cmd := exec.CommandContext(ctx, "git", "-C", cwd, "rev-parse", "--is-inside-work-tree")
	out, err := cmd.Output()
	if err != nil {
		if ctx.Err() != nil {
			return false, fmt.Errorf("runner: git work tree: %w", ctx.Err())
		}
		return false, nil
	}
	return strings.TrimSpace(string(out)) == "true", nil
}

func enumGit(ctx context.Context, cwd string, emit func(uploadJob) error) error {
	cmd := exec.CommandContext(ctx, "git", "-C", cwd, "ls-files", "-co", "--exclude-standard", "-z")
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return fmt.Errorf("runner: git ls-files: %w", err)
	}
	if err := cmd.Start(); err != nil {
		return fmt.Errorf("runner: git ls-files: %w", err)
	}
	scanErr := scanNUL(ctx, cwd, stdout, emit)
	if scanErr != nil {
		_, _ = io.Copy(io.Discard, stdout)
	}
	waitErr := cmd.Wait()
	if scanErr != nil {
		return scanErr
	}
	if waitErr != nil {
		return fmt.Errorf("runner: git ls-files: %w", waitErr)
	}
	return nil
}

func scanNUL(ctx context.Context, cwd string, r io.Reader, emit func(uploadJob) error) error {
	buf := make([]byte, 32*1024)
	var rest []byte
	for {
		if err := ctx.Err(); err != nil {
			return fmt.Errorf("runner: list files: %w", err)
		}
		n, err := r.Read(buf)
		rest = append(rest, buf[:n]...)
		if len(rest) > maxPathBuf {
			return errors.New("runner: git ls-files path record exceeds 8MiB")
		}
		var emitErr error
		rest, emitErr = emitReady(cwd, rest, emit)
		if emitErr != nil {
			return emitErr
		}
		if errors.Is(err, io.EOF) {
			return emitFinal(cwd, rest, emit)
		}
		if err != nil {
			return fmt.Errorf("runner: git ls-files: %w", err)
		}
	}
}

func emitReady(cwd string, rest []byte, emit func(uploadJob) error) ([]byte, error) {
	for {
		i := bytes.IndexByte(rest, 0)
		if i < 0 {
			return rest, nil
		}
		rel := string(rest[:i])
		rest = rest[i+1:]
		if err := emitRel(cwd, rel, emit); err != nil {
			return rest, err
		}
	}
}

func emitFinal(cwd string, rest []byte, emit func(uploadJob) error) error {
	if len(rest) == 0 {
		return nil
	}
	return emitRel(cwd, string(rest), emit)
}

func emitRel(cwd, rel string, emit func(uploadJob) error) error {
	if rel == "" || rel == ".dalgona-baseline" || rel == ".git" || strings.HasPrefix(rel, ".git/") {
		return nil
	}
	abs := filepath.Join(cwd, filepath.FromSlash(rel))
	info, err := os.Lstat(abs)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return fmt.Errorf("runner: stat %s: %w", abs, err)
	}
	if !info.Mode().IsRegular() {
		return nil
	}
	job := uploadJob{local: abs, remote: workspaceRoot + "/" + filepath.ToSlash(rel), perm: info.Mode().Perm()}
	return emit(job)
}

func enumWalk(cwd string, emit func(uploadJob) error) error {
	return filepath.WalkDir(cwd, func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return fmt.Errorf("runner: walk %s: %w", path, err)
		}
		return walkEntry(cwd, path, d, emit)
	})
}

func walkEntry(cwd, path string, d fs.DirEntry, emit func(uploadJob) error) error {
	if d.IsDir() {
		if d.Name() == ".git" {
			return filepath.SkipDir
		}
		return nil
	}
	info, err := d.Info()
	if err != nil {
		return fmt.Errorf("runner: stat %s: %w", path, err)
	}
	if !info.Mode().IsRegular() {
		return nil
	}
	rel, err := filepath.Rel(cwd, path)
	if err != nil {
		return fmt.Errorf("runner: rel %s: %w", path, err)
	}
	if rel == ".dalgona-baseline" {
		return nil
	}
	job := uploadJob{local: path, remote: workspaceRoot + "/" + filepath.ToSlash(rel), perm: info.Mode().Perm()}
	return emit(job)
}

func chmodExec(ctx context.Context, r Runner, paths []string) error {
	for len(paths) > 0 {
		n := chmodBatchSize
		if n > len(paths) {
			n = len(paths)
		}
		if err := chmodBatch(ctx, r, paths[:n]); err != nil {
			return err
		}
		paths = paths[n:]
	}
	return nil
}

func chmodBatch(ctx context.Context, r Runner, paths []string) error {
	argv := make([]string, 0, 4+len(paths))
	argv = append(argv, "sh", "-c", chmodScript, "_")
	argv = append(argv, paths...)
	_, err := runRemote(ctx, r, argv)
	return err
}

func writeBaseline(ctx context.Context, r Runner) (string, error) {
	out, err := runRemote(ctx, r, []string{"sh", "-c", treeScript})
	if err != nil {
		return "", err
	}
	sha := strings.TrimSpace(out)
	if sha == "" {
		return "", errors.New("runner: git write-tree returned empty baseline")
	}
	if writeErr := r.WriteFile(ctx, baselinePath, []byte(sha), 0o644); writeErr != nil {
		return "", fmt.Errorf("runner: write baseline: %w", writeErr)
	}
	return sha, nil
}

var runnerKillGrace = 2 * time.Second

const runnerRedialBudget = 2 * time.Second

// Redialer supplies a fresh connection for process teardown when the
// current stream is dead.
type Redialer interface {
	Redial(ctx context.Context) (Runner, error)
}

func StopProcessWithGrace(r Runner, id string, grace time.Duration) {
	ctx, cancel := context.WithTimeout(context.Background(), grace)
	defer cancel()
	stop, closeStop, termPending := stopTarget(ctx, r, id)
	defer closeStop()
	if termPending {
		_ = stop.Signal(ctx, id, SignalTerm)
	}
	var offset int64
	timer := time.NewTimer(pollInterval)
	defer timer.Stop()
	for ctx.Err() == nil {
		out, err := stop.Read(ctx, id, offset)
		if err != nil || out.Exit != nil {
			break
		}
		offset = nextOffset(offset, out)
		drainTimer(timer)
		timer.Reset(pollInterval)
		select {
		case <-ctx.Done():
		case <-timer.C:
		}
	}
	killCtx, killCancel := context.WithTimeout(context.Background(), time.Second)
	defer killCancel()
	_ = stop.Signal(killCtx, id, SignalKill)
}

func stopTarget(ctx context.Context, r Runner, id string) (Runner, func(), bool) {
	err := r.Signal(ctx, id, SignalTerm)
	if err == nil {
		return r, func() {}, false
	}
	fresh, ok := redialAfterClosedStream(r, err)
	if !ok {
		return r, func() {}, false
	}
	return fresh, func() { _ = fresh.Close() }, true
}

func redialAfterClosedStream(r Runner, termErr error) (Runner, bool) {
	if !errors.Is(termErr, errRemoteClosed) {
		return nil, false
	}
	rd, ok := r.(Redialer)
	if !ok {
		return nil, false
	}
	dialCtx, cancel := context.WithTimeout(context.Background(), runnerRedialBudget)
	defer cancel()
	fresh, err := rd.Redial(dialCtx)
	if err != nil {
		return nil, false
	}
	return fresh, true
}

func stopRunnerProcess(r Runner, id string) {
	StopProcessWithGrace(r, id, runnerKillGrace)
}

func runRemote(ctx context.Context, r Runner, argv []string) (string, error) {
	id, err := newProcID()
	if err != nil {
		return "", err
	}
	spec := ProcessSpec{ID: id, Argv: argv, Dir: workspaceRoot}
	if startErr := r.Start(ctx, spec); startErr != nil {
		return "", fmt.Errorf("runner: start process: %w", startErr)
	}
	out, code, err := waitExit(ctx, r, id)
	if err != nil {
		return "", err
	}
	if code != 0 {
		return "", fmt.Errorf("runner: process %s exited %d: %s", id, code, strings.TrimSpace(out))
	}
	return out, nil
}

func newProcID() (string, error) {
	var buf [8]byte
	if _, err := rand.Read(buf[:]); err != nil {
		return "", fmt.Errorf("runner: process id: %w", err)
	}
	return "proc-" + hex.EncodeToString(buf[:]), nil
}

func waitExit(ctx context.Context, r Runner, id string) (string, int, error) {
	var buf bytes.Buffer
	var offset int64
	timer := time.NewTimer(pollInterval)
	defer timer.Stop()
	for {
		out, err := r.Read(ctx, id, offset)
		if err != nil {
			stopRunnerProcess(r, id)
			return "", 0, fmt.Errorf("runner: read process %s: %w", id, err)
		}
		buf.Write(out.Data)
		offset = nextOffset(offset, out)
		if out.Exit != nil {
			return buf.String(), out.Exit.Code, nil
		}
		if err := waitPoll(ctx, r, id, timer); err != nil {
			return "", 0, fmt.Errorf("runner: process %s: %w", id, err)
		}
	}
}

func nextOffset(offset int64, out Output) int64 {
	if out.Next > offset {
		return out.Next
	}
	return offset + int64(len(out.Data))
}

func waitPoll(ctx context.Context, r Runner, id string, timer *time.Timer) error {
	drainTimer(timer)
	timer.Reset(pollInterval)
	select {
	case <-ctx.Done():
		stopRunnerProcess(r, id)
		return ctx.Err()
	case <-timer.C:
		return nil
	}
}

func drainTimer(timer *time.Timer) {
	if timer.Stop() {
		return
	}
	select {
	case <-timer.C:
	default:
	}
}
