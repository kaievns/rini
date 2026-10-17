## What now holds

<!-- One line: what holds after this change, and which test holds it. -->

## Checklist

The rules behind each box are in `CONTRIBUTING.md`.

- [ ] `bin/check.sh` passes. Its test counts, passed and ignored:
- [ ] Each new test fails with the change undone, and I watched it fail.
- [ ] A reported bug or request is a requirement in `specs/` with its **Reported** line.
- [ ] Every doc or spec this change makes stale is corrected in place, not appended to.
- [ ] No limit is raised, no exception or `#[allow]` added, and no check weakened.
- [ ] `SLIDE_SCORES` and `ZOOM_SCORES` are unchanged, or the description says why the look changed.
