# Running as a service

rini runs as a per-user launchd agent. This is what starting and restarting it must guarantee.

## Which build runs

- `rini service start` and `rini service restart` MUST put the service on the build that runs the
  command — the same file, with any symlink resolved — and on no other. Looking the binary up
  anywhere else can find a stale copy, and a stale copy is a different code identity: macOS then
  treats it as a client it never granted Accessibility or Screen Recording to, and it runs month-old
  behaviour while every check against the new build passes.
- The service MUST launch the resolved file rather than a symlink to it. macOS keys the grants to the
  launch path, and launched through a symlink rini behaved as an ungranted client.
- When starting or restarting changes which build the service points at, the change MUST take effect
  on that same command. launchd runs the job definition it loaded, not the file on disk, so a rewritten
  definition has to be unloaded and loaded again — restarting the loaded one runs the old build.
- Starting a service that is already running on the right build MUST leave it running. A start is not
  a restart.

> **Reported 2026-09-28.** "rini service start should pick the build we're running on now." Two
> deploys had run a stale `~/.local/bin/rini` found on the search path, one on 2026-09-15 and one on
> 2026-09-24, the second an August build. The second half — a rewritten definition not taking effect —
> was found while fixing the first.

## Where it lives

`src/app/launch_agent.rs` builds the definition and drives launchd. The measurements behind these
rules are in `docs/permissions-and-the-launch-agent.md`.
