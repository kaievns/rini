# Two questions that were left open, and what closed them

Both used to be `TODO`s in `platform/app_actor.rs`. Both were closed on 2026-09-28 without a code
change, on evidence. Recorded here so neither gets reopened on the strength of the old comment.

## Should a raise be verified and retried?

**No, not without an observed failure.**

`Event::RaiseCompleted` means the raise was issued, not that it landed. The old comment wanted the
actor to check the window server's frontmost window afterwards and retry. Three things weigh against
building that on speculation:

- **No failure has been observed.** The one report that looked like a raise not landing — "when I
  switch to the 1password window it doesn't go up" — turned out to be a floating window stranded at
  its off-screen park frame. It had been raised; it was just off screen (`specs/windows.md`).
- **A retry is another raise, with its own focus echoes.** One keypress has already been measured
  producing eleven completions and sweeping the strip across the workspace six times, before
  `RaiseEcho` learned to drop them ("The offset is honest, and it still moved eight times per press"
  in `src/animation/docs/capture-overlay-research.md`). Retries would feed that directly.
- **Checking costs a wait.** The window server reorders asynchronously, so a check straight after the
  raise reads the old order. Waiting happens inside the one mutex every raise takes, which delays every
  raise queued behind it.

If a raise that genuinely did not land is ever seen, the check belongs straight after the raise in
`handle_raise_request`, and the first step is to log it rather than retry, so there is data to pick a
policy from.

## Can the title-element list become a general rule?

**No. The signal only means something inside the two applications it names.**

`needs_title_element_to_be_standard` lists iTerm2 and the macOS text-cursor indicator
(`com.apple.TextInputUI.xpc.CursorUIViewService`): for those, a window with no readable
`AXTitleUIElement` is not a real window. The old comment wanted it generalised. Measured on this
machine, 2026-09-28, every window of every running application:

| Application | Subrole | `AXTitleUIElement` | Close button |
|---|---|---|---|
| Chrome, Outlook, VS Code, Slack, Obsidian, Zen, Messages, Kiro Crew | `AXStandardWindow` | absent | present |
| Ghostty | `AXStandardWindow` | present | present |
| Notification Center | `AXSystemDialog`, `AXUnknown` | absent | absent |

Real windows from eight of nine applications have no title element, so the general rule would turn
away nearly every window rini manages. The list stays a list.

Close-button presence does separate real windows from the Notification Center entries above, and is
the likelier general rule. It cannot be adopted on this evidence: neither listed application was
running, so there is no dump showing it catches what the list catches.
