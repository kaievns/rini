# Running as a service

rini runs as a per-user launchd agent. This is what starting and restarting it must guarantee.

## Which build runs

- `rini service start` and `rini service restart` MUST put the service on the build that runs the
  command — the same file, with any symlink resolved — and on no other, within the path rule below.
  Looking the binary up
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
- A start or restart from a binary at another path than the one the service runs MUST refuse, name
  both paths, and say what moving costs, unless it is given `--move`. macOS ties the grants to the
  path as well as the signature, so a build at a new path is a client it never granted, even signed
  with the same certificate and identifier. A service with no installed binary has no path to
  protect.
- A release build in the checkout MUST NOT replace the binary the service runs unless the build
  says it is a deploy, and MUST stop before it compiles any of rini's libraries or binaries, naming
  where other builds go and how to deploy. A `--target <triple>` release build MUST still compile,
  its host build scripts included. launchd relaunches the service's binary after any crash, so a compile
  check that lands there is a deploy nobody ran.

> **Reported 2026-09-29.** "you switched from a dev build to another thing that requires permission
> regrants. this happened like 10 times already". The latest was a rollback that pointed the service
> at an older build in `target-3ad0d60/` instead of moving that build into `target/release/rini`.

> **Reported 2026-09-28.** "rini service start should pick the build we're running on now." Two
> deploys had run a stale `~/.local/bin/rini` found on the search path, one on 2026-09-15 and one on
> 2026-09-24, the second an August build. The second half — a rewritten definition not taking effect —
> was found while fixing the first.

## A panic

- A panic anywhere MUST end rini, and launchd MUST relaunch it.
- Code outside a test MUST NOT catch a panic. The panic hook aborts the process before an unwind
  reaches a `catch_unwind`, so a catch reads as a thread that survives a panic and never runs.

## Where it lives

`src/app/launch_agent.rs` builds the definition and drives launchd, and `install_panic_hook` in
`src/main.rs` aborts on a panic. The rustc wrapper `bin/sign-rustc.sh` refuses a release build into
`target/release` or `target/release/deps` without `RINI_DEPLOY=1`, held by `tests/deploy_path.rs`.
The measurements behind these rules are in `docs/permissions-and-the-launch-agent.md`.
