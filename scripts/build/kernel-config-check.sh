#!/usr/bin/env bash
# Resolves scripts/build/kernel-fragment.config against x86_64_defconfig
# exactly the way assets.sh does -- defconfig, merge_config.sh -m,
# olddefconfig -- and reports every line of the fragment that did NOT
# survive that resolution. No compilation: seconds, not minutes.
#
# This exists because kconfig silently overrules a fragment in ways that
# are invisible until you inspect the built .config:
#
#   - `select` beats a user's n. CONFIG_VT selects INPUT, so asking for
#     INPUT=n while VT stays y is ignored.
#   - a symbol whose prompt is conditional ("bool ... if EXPERT") is not
#     user-settable when that condition is off, so it silently falls
#     back to its default -- which is how CONFIG_VT itself survived being
#     switched off.
#
# Both cost a full kernel rebuild to discover the slow way. Run this
# after editing the fragment and before `make distclean-kernel dist`.
set -euo pipefail

KERNEL_VERSION="${KERNEL_VERSION:-6.18.47}"

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
FRAGMENT="$HERE/scripts/build/kernel-fragment.config"
SRC_DIR="$HERE/target/kernel-build/linux-$KERNEL_VERSION"

if [ ! -f /run/.toolboxenv ]; then
    command -v toolbox &>/dev/null || { echo "cella: 'toolbox' not found -- run: make init" >&2; exit 1; }
    echo "cella: entering the cella-build toolbox to resolve the kernel config"
    exec toolbox run -c cella-build env KERNEL_VERSION="$KERNEL_VERSION" \
        "$HERE/scripts/build/kernel-config-check.sh"
fi

if [ ! -d "$SRC_DIR" ]; then
    echo "cella: no kernel source at $SRC_DIR -- run: make dist" >&2
    exit 1
fi

cd "$SRC_DIR"
echo "cella: resolving $KERNEL_VERSION config (defconfig + fragment + olddefconfig)"
make x86_64_defconfig >/dev/null
scripts/kconfig/merge_config.sh -m .config "$FRAGMENT" >/dev/null 2>&1
make olddefconfig >/dev/null

# Compare what the fragment asked for against what survived.
python3 - "$FRAGMENT" .config <<'PYEOF'
import re, subprocess, sys

fragment, config = sys.argv[1], sys.argv[2]

want = {}
for line in open(fragment):
    line = line.strip()
    if m := re.match(r'^(CONFIG_[A-Z0-9_]+)=(.+)$', line):
        want[m.group(1)] = m.group(2)
    elif m := re.match(r'^# (CONFIG_[A-Z0-9_]+) is not set$', line):
        want[m.group(1)] = 'n'

have = {}
for line in open(config):
    line = line.strip()
    if m := re.match(r'^(CONFIG_[A-Z0-9_]+)=(.+)$', line):
        have[m.group(1)] = m.group(2)
    elif m := re.match(r'^# (CONFIG_[A-Z0-9_]+) is not set$', line):
        have[m.group(1)] = 'n'

bad = []
for sym, expected in sorted(want.items()):
    # A symbol absent from .config entirely is off -- either not set or
    # compiled out by a dependency -- which satisfies a request for n.
    actual = have.get(sym, 'n')
    if actual != expected:
        bad.append((sym, expected, actual))

print(f"\n{len(want)} symbols requested by the fragment, {len(want) - len(bad)} took effect.")
if not bad:
    print("OK: every line of the fragment survived kconfig resolution.")
    sys.exit(0)

print("\nMISMATCH -- these fragment lines did NOT take effect:")
for sym, expected, actual in bad:
    print(f"\n  {sym}: asked for {expected}, got {actual}")
    short = sym[len("CONFIG_"):]

    # Diagnosed here rather than leaving a human to go grep the tree:
    # the three causes are distinguishable straight from the Kconfig files.
    stanza = subprocess.run(
        ["grep", "-rn", "--include=Kconfig*", f"^config {short}$", "."],
        capture_output=True, text=True,
    ).stdout.strip().splitlines()

    if not stanza:
        print(f"    CAUSE: no `config {short}` stanza exists in this kernel. The symbol")
        print( "           is misspelled or was removed upstream -- either way this")
        print( "           fragment line has never done anything at all.")
        continue

    path, lineno = stanza[0].split(":")[:2]
    print(f"    defined at {path}:{lineno}")
    lines = open(path).read().splitlines()
    for line in lines[int(lineno):int(lineno) + 6]:
        t = line.strip()
        if t.startswith(("bool", "tristate", "int", "string")):
            print(f"    prompt: {t}")
            if " if " in t:
                print(f"    CAUSE: the prompt is conditional on {t.split(chr(32)+chr(105)+chr(102)+chr(32), 1)[1]}.")
                print( "           With that condition off the symbol is not user-settable,")
                print( "           so it falls back to its default and the line is ignored.")
            break

    if expected == "n":
        sel = subprocess.run(
            ["grep", "-rn", "--include=Kconfig*", "-e", f"select {short}$", "-e", f"select {short} ", "."],
            capture_output=True, text=True,
        ).stdout.strip().splitlines()
        if sel:
            print(f"    CAUSE: {len(sel)} symbol(s) `select` it, and select beats n. First few:")
            for line in sel[:4]:
                print(f"           {line.strip()}")
            print( "           Turn those off (or make them unreachable) to release it.")

sys.exit(1)
PYEOF
