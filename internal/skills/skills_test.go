package skills_test

import (
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/skills"
)

func TestListFrontmatterFieldsAndPath(t *testing.T) {
	home := t.TempDir()
	cwd := t.TempDir()
	dir := filepath.Join(home, "skills", "ignored-dir")
	body := "---\r\nname: \"quoted\"\r\ndescription: 'A description'\r\nother: skip\r\n---\r\nbody\r\n"
	path := writeSkill(t, dir, body)
	got, err := skills.List(home, cwd, nil)
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 1 {
		t.Fatalf("got %d skills, want 1", len(got))
	}
	want := skills.Skill{Name: "quoted", Description: "A description", Path: path}
	if got[0] != want {
		t.Fatalf("got %#v, want %#v", got[0], want)
	}
}

func TestListNameFallsBackToDirectory(t *testing.T) {
	home := t.TempDir()
	cwd := t.TempDir()
	writeSkill(t, filepath.Join(home, "skills", "blank-name"), "---\nname:\n---\n")
	writeSkill(t, filepath.Join(home, "skills", "from-dir"), "---\ndescription: only\n---\n")
	writeSkill(t, filepath.Join(home, "skills", "quoted-empty"), "---\nname: \"\"\n---\n")
	got, err := skills.List(home, cwd, nil)
	if err != nil {
		t.Fatal(err)
	}
	want := []string{"blank-name", "from-dir", "quoted-empty"}
	if len(got) != len(want) {
		t.Fatalf("got %#v, want names %v", got, want)
	}
	for i := range want {
		if got[i].Name != want[i] {
			t.Fatalf("got names %#v, want %v", got, want)
		}
	}
}

func TestListHomeProjectExtraPrecedence(t *testing.T) {
	home := t.TempDir()
	cwd := t.TempDir()
	extra := t.TempDir()
	homePath := writeSkill(t, filepath.Join(home, "skills", "shared"), "---\nname: shared\n---\n")
	writeSkill(t, filepath.Join(cwd, ".dalgona", "skills", "shared"), "---\nname: shared\ndescription: project\n---\n")
	writeSkill(t, filepath.Join(extra, "shared"), "---\nname: shared\ndescription: extra\n---\n")
	projectPath := writeSkill(t, filepath.Join(cwd, ".dalgona", "skills", "only-project"), "---\nname: only-project\n---\n")
	extraPath := writeSkill(t, filepath.Join(extra, "only-extra"), "---\nname: only-extra\n---\n")
	got, err := skills.List(home, cwd, []string{extra})
	if err != nil {
		t.Fatal(err)
	}
	want := []skills.Skill{
		{Name: "shared", Path: homePath},
		{Name: "only-project", Path: projectPath},
		{Name: "only-extra", Path: extraPath},
	}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("got %#v, want ordered unique skills %#v", got, want)
	}
}

func TestListIgnoresMissingRootsAndNonDirectories(t *testing.T) {
	home := t.TempDir()
	cwd := t.TempDir()
	writeFile(t, filepath.Join(home, "skills"), "file")
	writeFile(t, filepath.Join(cwd, ".dalgona", "skills"), "file")
	extraFile := filepath.Join(t.TempDir(), "not-a-root")
	writeFile(t, extraFile, "x")
	extra := t.TempDir()
	writeFile(t, filepath.Join(extra, "note.txt"), "x")
	if err := os.MkdirAll(filepath.Join(extra, "no-skill"), 0o755); err != nil {
		t.Fatal(err)
	}
	path := writeSkill(t, filepath.Join(extra, "real"), "---\nname: real\n---\n")
	got, err := skills.List(home, cwd, []string{extraFile, extra, filepath.Join(home, "missing")})
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 1 || got[0].Path != path || got[0].Name != "real" {
		t.Fatalf("got %#v, want only %s", got, path)
	}
}

func TestListFollowsSkillDirectorySymlink(t *testing.T) {
	root := t.TempDir()
	home := filepath.Join(root, "home")
	target := filepath.Join(root, "target")
	writeSkill(t, target, "---\nname: linked\ndescription: target\n---\n")
	link := filepath.Join(home, "skills", "linked")
	if err := os.MkdirAll(filepath.Dir(link), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(target, link); err != nil {
		t.Skipf("directory symlinks are unavailable: %v", err)
	}
	got, err := skills.List(home, t.TempDir(), nil)
	if err != nil {
		t.Fatal(err)
	}
	want := []skills.Skill{{Name: "linked", Description: "target", Path: filepath.Join(link, "SKILL.md")}}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("got %#v, want linked skill %#v", got, want)
	}
}

func TestListRejectsMalformedFrontmatter(t *testing.T) {
	openHome := t.TempDir()
	openPath := writeSkill(t, filepath.Join(openHome, "skills", "bad-open"), "name: x\n")
	_, err := skills.List(openHome, t.TempDir(), nil)
	assertMalformed(t, err, openPath)

	closeHome := t.TempDir()
	closePath := writeSkill(t, filepath.Join(closeHome, "skills", "bad-close"), "---\nname: x\n")
	_, err = skills.List(closeHome, t.TempDir(), nil)
	assertMalformed(t, err, closePath)
}

func assertMalformed(t *testing.T, err error, path string) {
	t.Helper()
	if err == nil || !strings.Contains(err.Error(), "skills: malformed frontmatter") || !strings.Contains(err.Error(), path) {
		t.Fatalf("got error %v, want malformed frontmatter at %s", err, path)
	}
}

func writeSkill(t *testing.T, dir, body string) string {
	t.Helper()
	return writeFile(t, filepath.Join(dir, "SKILL.md"), body)
}

func writeFile(t *testing.T, path, body string) string {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
	return path
}
