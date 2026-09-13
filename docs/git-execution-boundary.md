# Git execution boundary

Engine Git handles disable executable repository settings, clear the environment
for local commands, and check for newly introduced driver names before each local
operation. Repository-local `include.path` and `includeIf.*.path` are refused:
including writable source files would expand the configuration inputs beyond the
protected Git metadata. Put repository settings directly in `config` or
`config.worktree`. Operator-global configuration and credentials remain available
to separately authorized network commands.

Preflight is not an immutable configuration snapshot. Enforced child sessions and
gates protect the common config, enabled worktree config, Git indirection files,
and metadata directory nodes. Layout validation refuses symlink inputs and
replaceable symlink ancestors; Unix also refuses multiply linked config inputs.
Layouts that would mount over authority directories are refused. A write grant
that covers this process's neutral global Git config is also refused, including
in a non-Git session; unrelated temporary directories remain usable.
Mount-based sandboxes pin directory nodes with writable mounts and overlay the
config files read-only, preserving Git's index, object, and ref lockfile writes.
An enabled but absent `config.worktree` within a writable root is refused on these
providers: create the intended regular file before running, or disable
`extensions.worktreeConfig`. With that extension disabled, the absent file stays
irrelevant while the enabling common config is protected. No host placeholder or
sanitized replacement config is written.

These protections apply to contained children. With sandbox enforcement off, or
when a separate unsandboxed host process can write the repository, configuration
can still change between preflight and Git's own read. Environment clearing
reduces inherited authority; neither re-reading config nor a hash closes that
race. Ordinary include expansion during Git's startup may still read its target
before Git processes `--no-includes`; invalid inputs fail closed and all config
subprocesses have the deadline below.

Every subprocess launched through `GitRepo`, including unhardened explicit
handles, has bounded raw
stdout and stderr capture. Limits are per subprocess:

| Command | Deadline | stdout | stderr |
| --- | --- | --- | --- |
| Configuration discovery/read | 10 seconds | 1 MiB | 2 MiB |
| Local repository operation | 120 seconds | 64 MiB | 2 MiB |
| Authorized network operation | 600 seconds | 64 MiB | 2 MiB |

Both streams drain concurrently. An output limit or deadline is an error, never a
successful truncated result. Cleanup kills the Unix process group or Windows Job
and reaps the direct child. Windows starts Git suspended and assigns its Job
before resuming the primary thread, so descendants cannot escape through the
assignment window. This is process supervision, not containment against a
program deliberately leaving its Unix process group.

Regression fixtures synchronize a contained writer after driver preflight and
before the real Git operation. They cover direct and worktree config, atomic
replacement, hardlinks, Git indirection files and directory renames, while a
normal contained commit verifies index/ref behavior. Supervisor fixtures verify
binary stream separation, limit boundaries, timeout and descendant cleanup,
including an exited leader whose descendant retains an output pipe.
