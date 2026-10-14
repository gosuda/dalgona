# git-checkpoint: List and restore git stash checkpoints.

## Ported from
pi `git-checkpoint`

## What it shows
A tool and a command that run a program through `tools.exec`. The model tool `checkpoint` and the command `/git-checkpoint:checkpoint` accept `action` (`list` or `restore`, default `list`) and `n` (default `0`); the command takes them as positional arguments. `list` runs `git stash list` and prints the result, or `no checkpoints` when it is empty. `restore` runs `git stash apply stash@{n}`. Every git run passes the approval ladder. During a turn, dal asks before it runs. A slash command runs outside a turn, where no one can answer, so its git run fails closed with the approval message.

## What differs from pi
This plugin only lists and restores. It does not create checkpoints. The pi hook runs `git stash create` at each turn start and offers to apply that stash when you branch from the turn. A failed git command stops the handler with its error.

## Install
Copy to the dal plugins data root and add `git-checkpoint` to `plugins`.

## Settings
None.
