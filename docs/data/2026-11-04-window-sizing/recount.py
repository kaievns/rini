import collections, csv, sys

BUILT_IN = "37D8832A-2D66-02CA-B9F7-8F30A301B230"
DISCARDED = ("64F4B5B7", "71A9490F")  # the two 1920x1200 displays, discarded by Kai on 2026-11-04
CLASS = {
    "wide": ["com.microsoft.VSCode", "dev.kiro.desktop", "app.zen-browser.zen", "com.google.Chrome",
             "com.tinyspeck.slackmacgap", "com.microsoft.Outlook", "com.amazon.kiro.crew",
             "com.shapr3d.shapr", "com.amazon.ACME"],
    "terminal": ["com.mitchellh.ghostty"],
    "narrow": ["com.apple.MobileSMS", "com.apple.iBooksX", "com.apple.finder", "com.microsoft.Word",
               "com.electron.dockerdesktop", "com.amazon.aia.mcs.desktop",
               "com.electron.amazonconnections", "com.apple.Terminal"],
    "per window": ["md.obsidian"],
    "floating": ["us.zoom.xos", "com.agilebits.onepassword7", "com.apple.systempreferences"],
}
APP_CLASS = {a: c for c, apps in CLASS.items() for a in apps}


def effective(width, dragged):
    if width == "None":
        return "default 1/2"
    if width.startswith("FullWidth"):
        return "full"
    r = 0.5 + float(width[len("Offset("):-1])
    if not dragged:
        r = min(max(r, 1 / 3), 2 / 3)
    for name, v in (("1/2", 1 / 2), ("2/3", 2 / 3), ("1/3", 1 / 3), ("full", 1.0)):
        if abs(r - v) < 1e-3:
            return name + (" dragged" if dragged else "")
    return f"{r:.3f}"


def kind(width):
    return "none" if width == "None" else ("full" if width.startswith("FullWidth") else "offset")


rows = list(csv.DictReader(open(sys.argv[1])))
kept = [r for r in rows if not any(d in r["arrangement"] for d in DISCARDED)]
print(f"file: {len(rows)} slots, {len(set(r['arrangement'] for r in rows))} arrangements")
print(f"kept: {len(kept)} slots, {len(set(r['arrangement'] for r in kept))} arrangements")
for a, n in collections.Counter(r["arrangement"] for r in kept).most_common():
    print(f"  {n:3} {a}")
built = [r for r in kept if r["display"] == BUILT_IN]
alone = [r for r in built if "+" not in r["arrangement"]]
print(f"built-in: {len(built)} ({len(alone)} laptop alone, {len(built) - len(alone)} docked); "
      f"other display: {len(kept) - len(built)}")
print(f"lab windows: {sum(r['lab'] == 'yes' for r in kept)} kept, "
      f"{sum(r['lab'] == 'yes' for r in built)} on the built-in")
docked = [r for r in kept if "+" in r["arrangement"]]
print(f"docked slots naming an external display: "
      f"{sum(r['display'] != BUILT_IN for r in docked)} of {len(docked)}")
print("width kinds, kept:", dict(collections.Counter(kind(r["width"]) for r in kept)))
print("Outlook, kept:", [(r["arrangement"][:8] + ("+" if "+" in r["arrangement"] else ""), r["width"])
                         for r in kept if r["app"] == "com.microsoft.Outlook"])
print("\nclass table: built-in, kept, non-lab")
sample = [r for r in built if r["lab"] == "no"]
for c in list(CLASS) + ["unclassed"]:
    rs = [r for r in sample if APP_CLASS.get(r["app"], "unclassed") == c]
    dist = collections.Counter(effective(r["width"], r["dragged"] == "yes") for r in rs)
    apps = sorted(set(r["app"] for r in rs))
    print(f"  {c:10} {len(rs):3}  {dict(dist.most_common())}  {apps if c == 'unclassed' else ''}")
print(f"  total {len(sample)}")
print("effective width, built-in non-lab:",
      dict(collections.Counter(effective(r["width"], r["dragged"] == "yes") for r in sample).most_common()))
