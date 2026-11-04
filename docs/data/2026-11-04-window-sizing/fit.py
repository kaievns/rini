import itertools, math

GAP = 3
BAR = 32
INCH = 25.4

# name: (looks-like width, height, physical width in mm or None, source)
DISPLAYS = {
    "14in": (1512, 982, 3024 / 254 * INCH, "assumed default mode; 3024px at 254ppi"),
    "16in": (1728, 1117, 344, "measured: CGDisplayScreenSize 344x222mm"),
    "27in 4K": (2560, 1440, 27 * 16 / math.hypot(16, 9) * INCH, "assumed looks-like 2560x1440"),
    "32in 4K": (3008, 1692, 697, "measured: window server plist mode; 697mm wide"),
    "5K2K": (3360, 1418, None, "Kai 2026-11-04: runs at 3360x1418; physical size unknown"),
}
# Assumed 5K2K panel widths, for the millimetre fit only: a 34in and a 40in 21:9 panel.
PANEL_5K2K = {"34in": 34 * 64 / math.hypot(64, 27) * INCH, "40in": 40 * 64 / math.hypot(64, 27) * INCH}

FR = {"1/4": 1 / 4, "1/3": 1 / 3, "1/2": 1 / 2, "2/3": 2 / 3, "1": 1.0}

# Kai's stated fraction per (class, display), 2026-11-04.
CELLS = {
    "editor": {"14in": "1", "16in": "1", "32in 4K": "1/2", "5K2K": "1/3"},
    "browser": {"14in": "1", "16in": "2/3", "32in 4K": "1/2", "5K2K": "1/3"},
    "chat": {"14in": "1", "16in": "1", "32in 4K": "1/2"},
    "terminal": {"14in": "1", "16in": "2/3", "27in 4K": "1/2", "32in 4K": "1/3", "5K2K": "1/3"},
    "narrow": {"14in": "1/2", "16in": "1/2", "32in 4K": "1/4", "5K2K": "1/4"},
}
MODEL_B = {"editor": "wide", "browser": "wide", "chat": "wide", "terminal": "terminal", "narrow": "narrow"}
TARGETS = {"wide": 1500, "terminal": 1230, "narrow": 830}
MIN_PRESET = 560


def col(length, r):
    v = length - 2 * GAP
    return v if r >= 1 else round(r * (v + GAP) - GAP)


def presets(d):
    w = DISPLAYS[d][0]
    return {p: col(w, r) for p, r in FR.items() if col(w, r) >= MIN_PRESET}


def snap(d, target, unit=lambda d, pt: pt):
    return min(presets(d).items(), key=lambda kv: abs(math.log(unit(d, kv[1]) / target)))[0]


def geomean(xs):
    return math.exp(sum(map(math.log, xs)) / len(xs))


print("== displays")
for d, (w, h, mm, src) in DISPLAYS.items():
    ppi = f"{w / (mm / INCH):.1f} pt/in" if mm else "pt/in unknown"
    print(f"{d:8} {w}x{h}  V={w - 2 * GAP}  column height={h - BAR - 2 * GAP}  {ppi}  [{src}]")
for name, mm in PANEL_5K2K.items():
    print(f"5K2K if a {name} panel ({mm:.0f}mm wide): {3360 / (mm / INCH):.1f} pt/in")

print("\n== presets per display, pt")
for d in DISPLAYS:
    print(f"{d:8} " + "  ".join(f"{p}={v}" for p, v in presets(d).items()))

print("\n== point widths Kai's fractions imply")
for c, m in CELLS.items():
    pts = [col(DISPLAYS[d][0], FR[f]) for d, f in m.items()]
    cells = "; ".join(f"{d} {f}={col(DISPLAYS[d][0], FR[f])}" for d, f in m.items())
    print(f"{c:9} {cells}  spread x{max(pts) / min(pts):.2f}")


def fit(unit, label):
    ins = loo = n = 0
    misses = []
    for c, m in CELLS.items():
        items = list(m.items())
        vals = [unit(d, col(DISPLAYS[d][0], FR[f])) for d, f in items]
        for i, (d, f) in enumerate(items):
            n += 1
            ins += snap(d, geomean(vals), unit) == f
            got = snap(d, geomean(vals[:i] + vals[i + 1:]), unit)
            loo += got == f
            if got != f:
                misses.append(f"{c}@{d}:{got}!={f}")
    print(f"{label:28} in sample {ins}/{n}  leave one out {loo}/{n}  misses: {', '.join(misses)}")


print("\n== snap test: one target per stated class (geometric mean), nearest preset by log distance")
fit(lambda d, pt: pt, "points")
for name, mm in PANEL_5K2K.items():
    def unit(d, pt, mm5=mm):
        width_mm = DISPLAYS[d][2] or mm5
        return pt / DISPLAYS[d][0] * width_mm
    fit(unit, f"millimetres, 5K2K {name}")


def score(targets, classes):
    hits, misses = 0, []
    for c, m in CELLS.items():
        for d, f in m.items():
            got = snap(d, targets[classes[c]])
            hits += got == f
            if got != f:
                misses.append(f"{c}@{d}:{got}!={f}")
    return hits, misses


print("\n== Model B, built-in targets", TARGETS)
hits, misses = score(TARGETS, MODEL_B)
print(f"{hits}/20  misses: {', '.join(misses)}")
for k, t in TARGETS.items():
    print(f"{k:9} " + "  ".join(f"{d}={snap(d, t)}" for d in DISPLAYS))
for d in DISPLAYS:
    h = DISPLAYS[d][1] - BAR - 2 * GAP
    rows = {p: col(h + 2 * GAP, r) for p, r in FR.items() if p in ("1/3", "1/2", "1")}
    best = min(rows.items(), key=lambda kv: abs(math.log(kv[1] / TARGETS["narrow"])))
    print(f"narrow height on {d}: {best[0]} ({best[1]}pt)")

print("\n== best targets, 10pt grid 500..2000")
grid = range(500, 2001, 10)
for name, classes in (
    ("three numbers (wide, terminal, narrow)", MODEL_B),
    ("four numbers (browser apart)", {**MODEL_B, "browser": "browser"}),
):
    keys = sorted(set(classes.values()))
    best = {}
    for k in keys:
        for t in grid:
            h = sum(snap(d, t) == f for c, m in CELLS.items() if classes[c] == k for d, f in m.items())
            lo, hi, top = best.get(k, (t, t, -1))
            if h > top:
                best[k] = (t, t, h)
            elif h == top:
                best[k] = (lo, t, h)
    print(name)
    for k, (lo, hi, h) in best.items():
        n = sum(len(m) for c, m in CELLS.items() if classes[c] == k)
        print(f"  {k:9} {h}/{n} for targets {lo}..{hi} (first run of the best score)")
    total = sum(h for _, _, h in best.values())
    print(f"  total {total}/20")

print("\n== Model B with display rules: browser 1/3 on displays at least 3200pt wide, "
      "Ghostty 1/3 on displays at least 2800pt wide")
hits = 0
misses = []
for c, m in CELLS.items():
    for d, f in m.items():
        got = snap(d, TARGETS[MODEL_B[c]])
        if c == "browser" and DISPLAYS[d][0] >= 3200:
            got = "1/3"
        if c == "terminal" and DISPLAYS[d][0] >= 2800:
            got = "1/3"
        hits += got == f
        if got != f:
            misses.append(f"{c}@{d}:{got}!={f}")
print(f"{hits}/20  misses: {', '.join(misses)}")

print("\n== moving under B: a width carried as points to another display and back")
bad = trips = 0
for a, b in itertools.permutations(DISPLAYS, 2):
    for p in presets(a):
        trips += 1
        there = snap(b, presets(a)[p])
        back = snap(a, presets(b)[there])
        if back != p:
            bad += 1
            print(f"  {a} {p} -> {b} {there} -> {a} {back}")
print(f"trips that do not return: {bad} of {trips}")
