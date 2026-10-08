"""sls-graph.py <fn,fn,...> [depth]: which SkyLight calls reach the connection's CA-transaction sync.

Stops a throwaway process (`same`, built from same.m; path in $SAME) once SkyLight is loaded,
disassembles each named call and its SkyLight callees to <depth>, and prints the first paths that
reach a sink. Reads only."""
import os
import re
import sys

sys.path.insert(0, "/Library/Developer/CommandLineTools/Library/PrivateFrameworks/LLDB.framework/Resources/Python")
import lldb

ROOTS = sys.argv[1].split(",")
DEPTH = int(sys.argv[2]) if len(sys.argv) > 2 else 4
SINKS = {
    "SLSConnectionSynchronizeSLSCATransaction",
    "SLSTransactionWait",
    "_SLSTransactionWaitSource",
    "run_query",
    "SLSConnectionSetLastSLSCATransaction",
    "CGSWindowSynchronizeSLSCATransaction",
}

dbg = lldb.SBDebugger.Create()
dbg.SetAsync(False)
target = dbg.CreateTarget(os.environ.get("SAME", "./same"))
target.BreakpointCreateByName("SLSMainConnectionID")
err = lldb.SBError()
info = lldb.SBLaunchInfo(["1"])
process = target.Launch(info, err)
assert process.GetState() == lldb.eStateStopped, err

ci = dbg.GetCommandInterpreter()
cache = {}


def callees(name):
    if name in cache:
        return cache[name]
    res = lldb.SBCommandReturnObject()
    ci.HandleCommand(f"disassemble -n {name}", res)
    out = res.GetOutput() or ""
    found = []
    for line in out.splitlines():
        m = re.search(r"\b(bl|b)\s+0x[0-9a-f]+\s+;\s+(.+)$", line)
        if not m:
            continue
        sym = m.group(2).strip()
        if sym.startswith("symbol stub for:") or sym.startswith("<+"):
            continue
        sym = sym.split(" ")[0]
        found.append(sym)
    cache[name] = (bool(out.strip()), list(dict.fromkeys(found)))
    return cache[name]


def search(root):
    paths = []
    stack = [(root, [root])]
    seen = set()
    while stack:
        name, path = stack.pop()
        if len(path) > DEPTH + 1:
            continue
        ok, cs = callees(name)
        for c in cs:
            if c in SINKS:
                paths.append(path + [c])
                continue
            if c in seen:
                continue
            seen.add(c)
            stack.append((c, path + [c]))
    return paths


for root in ROOTS:
    ok, cs = callees(root)
    if not ok:
        print(f"{root}: NOT FOUND")
        continue
    paths = search(root)
    if paths:
        print(f"{root}: SYNCS  " + " | ".join(" -> ".join(p) for p in paths[:3]))
    else:
        print(f"{root}: no sync within depth {DEPTH} (direct callees: {', '.join(cs[:12])})")

process.Kill()
