def has_shebang(result, workspace):
    return result["exit"] == "ok" and workspace.read("scripts/run.sh").startswith("#!/bin/sh")

evalpack.pack(name = "edit-anchor-smoke", cases = [
    evalpack.case(
        id = "shebang",
        prompt = "Add a shebang line to scripts/run.sh so it runs as a shell script.",
        files = {"scripts/run.sh": "set -e\necho ok\n"},
        check = has_shebang,
        repeats = 3,
    ),
])
