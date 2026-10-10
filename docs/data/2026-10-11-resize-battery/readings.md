# Navigation timing after the resize work (2026-10-11)

The timing battery (`tools/focuslab/battery.sh`, README "The battery") flown on the build before the
resize work (258de683, `o1`..`o6`) and on main at d9461781 (`n1`..`n6`), one session, the same scene
(every pan segment checked against `n1`'s, parked windows by size), Kai away.

- Old against new, the two-stage rule (`compare.csv`, `compare.err`): 81 of 87 pass, 6 fail stage 2:
  `pan.overlay_up.p90` 64.8 against the old 62.6ms, `pan.lift.p90` 427.8 against 424.7ms, the switch
  frame interval p99 and max 24.7 against 22.8ms, the spare pans' interval p90 22.4 against 22.3ms
  and every flight's interval p50 17.34 against 17.33ms.
- One build against itself, stage 1 only (`null-old.*`, `null-new.*`): the old build fails 9 and 20
  metrics against its own runs, the new 15 and 22, either way round. So 6 stage-2 fails, none of the
  same metrics as the 6 of the 2026-10-10 battery on a different scene, is within what the rule
  fails by chance; no metric moves by more than 3ms.
- Every run wears pictures over 5s old, 25-37 a run on either build (the switch flies other
  workspaces' windows from the cache); `n1`, the first after a launch, wore one 211s old.
