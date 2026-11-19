### Outside checks of the code-rewrite workstream

- Check: in a live `dal` session with `ask` approval, the rewrite diff arrives as `item/fileChange/patchUpdated` before the approval request, and the approve action applies the file. Guards: Code rewrite step 6. Why it is outside: the check needs a running daemon, a client and a model server, and this plan cannot start them. Fallback: record the check as Not proven, and keep `TestRewriteAskApprovalSendsDiff` as the evidence.

