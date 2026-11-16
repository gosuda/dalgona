import pathlib
import subprocess
import sys
import tempfile
import unittest

CHECKER = pathlib.Path(__file__).resolve().with_name("check_style.py")
APPEND_LINE = "        current.append((number, line))"
TABLE_ROW_LINE = "            result.append([(number, line)])"


def words(count):
    return " ".join(f"w{i}" for i in range(count))


def sentence(count):
    return words(count) + "."


class StyleCheckerTest(unittest.TestCase):
    def run_checker(self, checker, target):
        return subprocess.run(
            [sys.executable, str(checker), str(target)],
            capture_output=True,
            text=True,
            check=False,
        )

    def write(self, text):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        path = pathlib.Path(directory.name) / "fixture.md"
        path.write_text(text, encoding="utf-8")
        return path

    def mutated(self, old, new):
        source = CHECKER.read_text(encoding="utf-8")
        mutated = source.replace(old, new)
        self.assertNotEqual(mutated, source)
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        path = pathlib.Path(directory.name) / "check_style_mutated.py"
        path.write_text(mutated, encoding="utf-8")
        return path

    def test_prose_sentence_limit_boundary(self):
        clean = self.write(sentence(25) + "\n")
        self.assertEqual(self.run_checker(CHECKER, clean).returncode, 0)
        long = self.write(sentence(26) + "\n")
        result = self.run_checker(CHECKER, long)
        self.assertEqual(result.returncode, 1)
        self.assertIn(f"{long}:1: 26 words, limit 25", result.stdout)

    def test_wrapped_sentence_accumulates_across_lines(self):
        clean = self.write(words(12) + "\n" + words(13) + ".\n")
        self.assertEqual(self.run_checker(CHECKER, clean).returncode, 0)
        wrapped = self.write(words(13) + "\n" + words(13) + ".\n")
        result = self.run_checker(CHECKER, wrapped)
        self.assertEqual(result.returncode, 1)
        self.assertIn(f"{wrapped}:1: 26 words, limit 25", result.stdout)

    def test_numbered_step_limit_single_line(self):
        clean = self.write("2. " + sentence(20) + "\n")
        self.assertEqual(self.run_checker(CHECKER, clean).returncode, 0)
        long = self.write("2. " + sentence(21) + "\n")
        result = self.run_checker(CHECKER, long)
        self.assertEqual(result.returncode, 1)
        self.assertIn(f"{long}:1: 21 words, limit 20", result.stdout)

    def test_numbered_step_limit_crosses_continuation(self):
        clean = self.write("1. " + words(11) + "\n" + words(9) + ".\n")
        self.assertEqual(self.run_checker(CHECKER, clean).returncode, 0)
        long = self.write("1. " + words(12) + "\n" + words(9) + ".\n")
        result = self.run_checker(CHECKER, long)
        self.assertEqual(result.returncode, 1)
        self.assertIn(f"{long}:1: 21 words, limit 20", result.stdout)

    def test_table_cell_semicolon_finds_row(self):
        clean = self.write("| ID | Result |\n|---|---|\n| T-01 | Reject. Never retarget. |\n")
        self.assertEqual(self.run_checker(CHECKER, clean).returncode, 0)
        marked = self.write("| ID | Result |\n|---|---|\n| T-01 | Reject; never retarget. |\n")
        result = self.run_checker(CHECKER, marked)
        self.assertEqual(result.returncode, 1)
        self.assertIn(f"{marked}:3: semicolon outside code", result.stdout)

    def test_table_cell_sentence_limit(self):
        fixture = self.write("| ID | Result |\n|---|---|\n| T-03 | " + sentence(26) + " |\n")
        result = self.run_checker(CHECKER, fixture)
        self.assertEqual(result.returncode, 1)
        self.assertIn(f"{fixture}:3: 26 words, limit 25", result.stdout)

    def test_table_code_span_holds_pipes_and_semicolon(self):
        fixture = self.write("| ID | Result |\n|---|---|\n| T-02 | Value `{a;b|c}` held. |\n")
        result = self.run_checker(CHECKER, fixture)
        self.assertEqual(result.returncode, 0, result.stdout)

    def test_inline_code_semicolon_allowed(self):
        fixture = self.write("Use `a;b` in prose.\n")
        self.assertEqual(self.run_checker(CHECKER, fixture).returncode, 0)

    def test_inline_code_span_across_lines_holds_semicolon(self):
        fixture = self.write("Use `a b\nc;d` here.\n")
        result = self.run_checker(CHECKER, fixture)
        self.assertEqual(result.returncode, 0, result.stdout)
        marked = self.write("Use `a b\nc;d` here;\n")
        result = self.run_checker(CHECKER, marked)
        self.assertEqual(result.returncode, 1)
        self.assertIn(f"{marked}:2: semicolon outside code", result.stdout)

    def test_inline_code_span_across_lines_counts_as_one_word(self):
        clean = self.write(words(24) + " `p q\nr s`.\n")
        self.assertEqual(self.run_checker(CHECKER, clean).returncode, 0)
        long = self.write(words(25) + " `p q\nr s`.\n")
        result = self.run_checker(CHECKER, long)
        self.assertEqual(result.returncode, 1)
        self.assertIn(f"{long}:1: 26 words, limit 25", result.stdout)

    def test_fenced_code_is_excluded(self):
        fixture = self.write("```text\na ; b — should " + words(29) + ".\n```\n")
        self.assertEqual(self.run_checker(CHECKER, fixture).returncode, 0)

    def test_em_dash_is_found(self):
        dashed = self.write("One word—two words here.\n")
        result = self.run_checker(CHECKER, dashed)
        self.assertEqual(result.returncode, 1)
        self.assertIn(f"{dashed}:1: em dash", result.stdout)

    def test_lower_case_should_is_found(self):
        fixture = self.write("The tool should refuse.\n")
        result = self.run_checker(CHECKER, fixture)
        self.assertEqual(result.returncode, 1)
        self.assertIn(f"{fixture}:1: lower-case should", result.stdout)

    def test_headings_separators_and_quotes_are_not_prose(self):
        text = (
            "# " + words(30) + "\n"
            "---\n"
            + sentence(20) + "\n\n" + sentence(20) + "\n"
            "> quoted; text — should\n"
        )
        fixture = self.write(text)
        self.assertEqual(self.run_checker(CHECKER, fixture).returncode, 0)

    def test_absent_target_fails(self):
        missing = pathlib.Path(tempfile.gettempdir()) / "style-checker-absent-target.md"
        self.assertFalse(missing.exists())
        result = self.run_checker(CHECKER, missing)
        self.assertEqual(result.returncode, 2)
        self.assertIn("target not found", result.stderr)

    def test_directory_without_specifications_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            pathlib.Path(directory, "notes.txt").write_text("x;y — should\n", encoding="utf-8")
            result = self.run_checker(CHECKER, pathlib.Path(directory))
            self.assertEqual(result.returncode, 2)
            self.assertIn("no specification files", result.stderr)

    def test_directory_with_clean_file_passes(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "spec.md"
            path.write_text(sentence(10) + "\n", encoding="utf-8")
            result = self.run_checker(CHECKER, pathlib.Path(directory))
            self.assertEqual(result.returncode, 0, result.stdout)

    def test_line_local_counting_mutant_misses_wrapped_sentence(self):
        fixture = self.write(words(13) + "\n" + words(13) + ".\n")
        self.assertEqual(self.run_checker(CHECKER, fixture).returncode, 1)
        mutant = self.mutated(APPEND_LINE, "        flush()\n" + APPEND_LINE)
        result = self.run_checker(mutant, fixture)
        self.assertEqual(result.returncode, 0, result.stdout)

    def test_table_skipping_mutant_misses_cell(self):
        fixture = self.write("| ID | Result |\n|---|---|\n| T-01 | Reject; never retarget. |\n")
        self.assertEqual(self.run_checker(CHECKER, fixture).returncode, 1)
        mutant = self.mutated(TABLE_ROW_LINE, "            pass")
        result = self.run_checker(mutant, fixture)
        self.assertEqual(result.returncode, 0, result.stdout)


if __name__ == "__main__":
    unittest.main()
