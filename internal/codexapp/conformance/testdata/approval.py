import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(os.environ["CONFORMANCE_SDK_DIR"]) / "src"))

from openai_codex.api import Thread
from openai_codex.client import CodexClient, CodexConfig
from openai_codex.generated.v2_all import CommandExecutionThreadItem, TurnStatus

requests = []


def approve(method, params):
    requests.append(method)
    if method != "item/commandExecution/requestApproval":
        raise RuntimeError(f"unexpected server request {method}: {params}")
    return {"decision": "accept"}


config = CodexConfig(
    codex_bin=os.environ["CONFORMANCE_CODEX_BIN"],
    cwd=os.environ["CONFORMANCE_WORKSPACE"],
)
with CodexClient(config=config, approval_handler=approve) as client:
    client.initialize()
    started = client.thread_start({"approvalPolicy": "untrusted"})
    result = Thread(client, started.thread.id).run(sys.argv[1])
    if result.status != TurnStatus.completed:
        raise RuntimeError(f"turn ended with {result.status}: {result.error}")
    outputs = [
        item.root.aggregated_output or ""
        for item in result.items
        if isinstance(item.root, CommandExecutionThreadItem)
    ]
    print("approval requests:", len(requests))
    print("command output:", "".join(outputs))
    print("final:", result.final_response)
