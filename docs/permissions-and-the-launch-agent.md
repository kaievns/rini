# Permissions and the launch agent

Why rini works when launched from a terminal and does not work when launchd
starts it. All of this is measured on this machine, macOS Darwin 25.6.

## Accessibility trust is inherited from the launching app

This is the finding that reframes everything else. A Swift script that has never
been added to the Accessibility list, run from a terminal:

```
$ swift /tmp/axtest.swift
executable: /tmp/axtest.swift
AXIsProcessTrusted(): true
```

The grant does not belong to that file. It belongs to the application that
launched it, and every process started from that terminal borrows it. So a
terminal launch of rini is trusted no matter how rini is signed, and a launchd
launch of the same binary is not trusted at all:

```
launchd-spawned /Users/kaievns/.local/bin/rini
  -> "Accessibility permission is not granted; prompting user for permission now."
     repeating every 30 seconds, no windows managed
```

Same path, same ad-hoc signature, same binary inode. The only difference is who
started it.

### What this corrects

The re-granting that seemed to follow every rebuild was being read as "ad-hoc
signing changes the cdhash, which invalidates the TCC grant". That is true in
general but it was not what was happening here: a terminal-launched rini never
needed its own grant, so re-signing it could not have taken one away. The
prompts were coming from launchd-started instances.

### What the launch agent therefore needs

Its own entry in System Settings, Privacy and Security, Accessibility, added
with `+` and pointing at the real binary:

```
/Users/kaievns/.local/bin/rini
```

That grant IS keyed to the binary, so it breaks whenever the binary is rebuilt,
because an ad-hoc signature has no stable identity across builds. This is the
concrete reason a real signing identity is worth having: with one, the grant
survives rebuilds. Without one, every install needs a re-grant, and that cost
falls on the launch agent, not on terminal launches.

## TCC keys the grant to the launch path, not the inode

Separate from the above, and also measured. The agent used to point at
`/opt/homebrew/bin/rini`, a symlink to `~/.local/bin/rini`. Pointed at the
symlink it behaved as an ungranted client even in states where the real path
worked, so `find_rini_executable` now canonicalises. The symlink is the more
stable path, which is why it was chosen originally, but stability is worth
nothing against the agent being unable to move a window.

## The generated plist had literal backslashes

The template is a Rust raw string, so its quotes need no escaping, but they were
escaped anyway and the backslashes were emitted verbatim:

```
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<plist version=\"1.0\">
```

That is not well-formed XML, and a strict parser rejects it:

```
xml.parsers.expat.ExpatError: XML declaration not well-formed: line 1, column 14
```

Apple's parser accepts it, so `plutil -lint` reported the installed file as `OK`
and this went unnoticed. `plutil -lint` is not a well-formedness check.

## The CLI cannot reach a launchd-started rini

Still open. Under launchd the agent starts and logs no error, but:

```
$ rini-cli query workspaces
Communication error: Rini's Mach service is not registered
```

rini registers its Mach service in its own bootstrap domain. A terminal-launched
rini and a terminal-launched `rini-cli` share that domain, so the lookup
succeeds; a launchd-started agent does not share it with the user's shell.

The fix is the `MachServices` key, which is present but commented out in
`src/sys/service.rs`. It is not a one-line change: with `MachServices` launchd
owns the port and hands it over, so the process has to `bootstrap_check_in`
rather than register its own. Until that is done, rini under launchd manages
windows but cannot be driven by the CLI, which also breaks the sketchybar
subscriptions.

## Current state

rini runs as a terminal-launched process, which has Accessibility by inheritance
and a reachable Mach service. It does not survive a reboot. Making it survive
one needs both of the open items above: an explicit Accessibility grant for the
binary, and `MachServices` check-in so the CLI still works.
