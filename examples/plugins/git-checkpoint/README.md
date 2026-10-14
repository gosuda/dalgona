# git-checkpoint: List and restore git stash checkpoints.

## Ported from
pi `git-checkpoint`

## What it shows
A command that runs a program through `tools.exec`. The command `/git-checkpoint:checkpoint` accepts positional arguments `action` (`list` or `restore`, default `list`) and `n` (default `0`). `list` runs `git stash list` and prints the result, or `no checkpoints` when it is empty. `restore` runs `git stash apply stash@{n}`. Every run passes the approval ladder, so dal asks before it runs a command.

## What differs from pi
This plugin only lists and restores. It does not create checkpoints. The pi hook runs `git stash create` at each turn start and offers to apply that stash when you branch from the turn. A failed git command stops the handler with its error.

## Install
Copy to the dal plugins data root and add `git-checkpoint` to `plugins`.

## Settings
None.
