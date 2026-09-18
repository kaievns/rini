# Permissions and the launch agent

What a launchd-started rini needs that a terminal-launched one gets for free.
All of this is measured on this machine, macOS Darwin 25.6.

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

## The CLI reaches a launchd-started rini

An earlier build failed with:

```
$ rini-cli query workspaces
Communication error: Rini's Mach service is not registered
```

and this was read as the launchd agent registering its Mach service in a
bootstrap domain the user's shell does not share. That reading is not borne
out. Measured 2026-09-18: a rini started by `rini service restart` (launchd,
`gui/<uid>` domain) logs `mach_server_begin: registered 'git.kaievns.rini' in
current bootstrap domain` and `rini-cli query workspaces` from a shell reaches
it, as do the four sketchybar `subscribe` hooks in the user config. The cause of
the earlier failure was not established; a stale binary at a second path (see
"Deploy is `service restart`") is the likeliest candidate.

`MachServices` stays commented out in `crates/rini-macos/src/service.rs`. Enabling
it would make launchd own the port and start rini on demand, which needs
`bootstrap_check_in` instead of `bootstrap_register`. Not needed for the CLI to
work.

## Deploy is `service restart`, never `stop` then `start`

Measured 2026-09-15, 1:39 UTC. `rini service start` regenerates the plist from
a PATH lookup. It found a stale `~/.local/bin/rini` and launched it. Same
identifier, different code requirement: TCC invalidated both grants for the
client. Accessibility prompted again; Screen Recording stayed revoked, so every
capture returned nothing and the overlay flew empty tiles (`tiles=0,
missing=22` on every flight). `service restart` is `kickstart -k` and leaves the
plist alone. Delete stale copies of the binary so a lookup cannot find them.

## Current state

rini runs as the launchd agent, deployed with `rini service restart`, with its
own Accessibility and Screen Recording grants keyed to the signed binary
(`docs/signing.md`). The CLI and sketchybar hooks reach it. Whether the grants
survive a reboot has not been measured since the signing identity was
introduced.

## A revoked screen-recording grant froze all input

Observed: removing rini's Screen Recording entry in System Settings while rini
runs froze every mouse click and key press in the session. The screen kept
compositing — animation in other windows was visible — but nothing responded,
including the TCC dialog that would have fixed it. Only a reboot recovered.

The mechanism, in three parts:

1. rini owns two ACTIVE CGEventTaps: the session tap (all mouse buttons, plus
   keyboard whenever any hotkey is bound — effectively always) and the HID
   gesture tap. An active tap sits in the delivery path: the window server
   holds each matching event until the tap's callback answers.
2. The callbacks make synchronous SkyLight calls per event (hit tests,
   occlusion checks) over the same SLS connection the capture paths use. When
   tccd re-evaluates the process after the grant change, those calls stall —
   and the callback stalls holding an event, so all input queues behind it.
3. macOS has a safety valve: it disables an unresponsive tap
   (`kCGEventTapDisabledByTimeout`) and lets input flow again. rini defeated
   it — the tap trampoline re-enabled the tap unconditionally, inside the
   callback, the moment its thread could breathe. Stalled tap back in, another
   timeout round, forever. The freeze survives until reboot because killing
   rini needs input, and launchd would resurrect it anyway
   (`KeepAlive Crashed=true`).

The fix moves re-enabling out of the callback entirely. A disable is now only
REPORTED to the owning actor, which re-arms under `ReEnableGovernor`: an
isolated disable re-enables immediately (wake from sleep, a one-off stall); a
third disable within 30s means the tap is genuinely stuck, so it stands down
for 10s per round — input flows without rini while the stall lasts, hotkeys
return when it clears. The reporting itself is the backpressure: a truly stuck
input thread cannot deliver the message, so the tap stays down exactly as long
as the thread is unhealthy.

Residual, deliberately not addressed here: the synchronous SLS hit tests
inside the tap callbacks are still the thing that stalls. Moving them off the
event path (or onto a separate SLS connection) would remove the freeze
trigger rather than the freeze amplifier, and is queued as follow-up work.
