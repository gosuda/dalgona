import os
import runpy
import sys
from pathlib import Path

examples = Path(os.environ["CONFORMANCE_SDK_DIR"]) / "examples"
sys.path.insert(0, str(examples))

import _bootstrap

_bootstrap.ensure_local_sdk_src()

from openai_codex import CodexConfig


def runtime_config():
    return CodexConfig(
        codex_bin=os.environ["CONFORMANCE_CODEX_BIN"],
        cwd=os.environ["CONFORMANCE_WORKSPACE"],
    )


_bootstrap.runtime_config = runtime_config
runpy.run_path(str(examples / sys.argv[1] / "sync.py"), run_name="__main__")
