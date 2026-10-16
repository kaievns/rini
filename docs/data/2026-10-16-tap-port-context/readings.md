# A tap port's context info is not the refcon

Measured 2026-10-16 on macOS 26, with a listen-only mouse-moved session tap made with refcon
`0x1234`. Build each with `clang -framework ApplicationServices -framework CoreFoundation`.

| probe | refcon | what the port gives back |
|---|---|---|
| `context.c`: `CFMachPortGetContext(port).info` | `0x1234` | `0x100969150` |
| `invalidation.c`: the invalidation callback's `info` | `0x1234` | `0x102885930` |

The port's context belongs to Core Graphics, so an invalidation callback that reads its `info` as
the refcon reads Core Graphics' record.
