#!/usr/bin/env python3
"""Generates gc-sys's src/generated.rs from the Guardian's Crusade decomp (LT-104).

    python3 app/crates/gc-sys/gen/gen.py --decomp ../psx-dev/gc_decomp [--check]

Needs Python 3.11+ and a built decomp (`make` there): it reads
- the link maps (build/main/slus_008.11.map, build/ovrNN/ovrNN.map) for every function;
- config/ovr/ovrNN.yaml and disk/OVR/OVRNN.BIN for the overlays' slots and their signatures;
- config/symbol_addrs.txt for named globals;
- the struct definitions (include/*.h, typedefs in src/), measured with the decomp's own matched
  compiler (tools/gcc/gcc-2.7.2-cdk-macos/cc1).

It checks layouts.toml against all that, then writes src/generated.rs. The output depends only on
its inputs, so running it again on an unchanged decomp changes nothing. --check compares instead
of writing and exits 1 on a difference.
"""

import argparse
import os
import re
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent
CRATE = HERE.parent
SPEC = CRATE / "layouts.toml"
OUT = CRATE / "src" / "generated.rs"

TYPES = {
    "u8": ("u8", 1), "i8": ("i8", 1), "u16": ("u16", 2), "i16": ("i16", 2), "u32": ("u32", 4), "i32": ("i32", 4),
    "matrix": ("Matrix", 0x20), "svector": ("SVector", 8), "vector": ("Vector", 0x10),
}
OVERLAYS = range(1, 26)


def fail(msg):
    print(f"gen.py: {msg}", file=sys.stderr)
    sys.exit(2)


def rust_type(t):
    m = re.fullmatch(r"(\w+)\[(\d+)\]", t)
    if m:
        inner, size = rust_type(m.group(1))
        n = int(m.group(2))
        return f"[{inner}; {n}]", size * n
    if t not in TYPES:
        fail(f"unknown type {t}")
    return TYPES[t]


# ---------------------------------------------------------------------------- link maps

SECTION = re.compile(r"^ (\.\w+)\s+0x([0-9a-f]+)\s+0x([0-9a-f]+)\s+(\S+)")
SYMBOL = re.compile(r"^\s+0x([0-9a-f]+)\s+([A-Za-z_][\w$]*)\s*$")


def functions(map_path, named, image=None):
    """[(addr, size, name)] of every function in the map's .text input sections, sorted, one name
    per address. Library objects list no symbols in the map, so `named` (address -> name, from the
    decomp's symbol files) fills them in where an address falls in a .text section, or anywhere in
    `image` (an overlay's load range: OVR01 links its CD library in as raw words after its .text)."""
    sections, found = [], {}
    sec = None
    for line in map_path.read_text(errors="ignore").splitlines():
        m = SECTION.match(line)
        if m:
            start, size = int(m.group(2), 16) & 0xFFFFFFFF, int(m.group(3), 16)
            sec = (start, start + size) if m.group(1) == ".text" and size else None
            if sec:
                sections.append(sec)
            continue
        if line and not line[0].isspace():
            sec = None
            continue
        m = SYMBOL.match(line)
        if m and sec:
            addr = int(m.group(1), 16) & 0xFFFFFFFF
            if sec[0] <= addr < sec[1]:
                found.setdefault(addr, []).append(m.group(2))
    if image:
        top = max((b for _, b in sections), default=image[0])
        if top < image[1]:
            sections.append((top, image[1]))
    for addr, name in named.items():
        if any(a <= addr < b for a, b in sections):
            found.setdefault(addr, []).append(name)
    result = []
    addrs = sorted(found)
    for i, a in enumerate(addrs):
        names = found[a]
        # A real name over a placeholder at the same address.
        name = next((n for n in names if not n.startswith(("FUN_", "func_"))), names[0])
        end = next(b for s, b in sections if s <= a < b)
        if i + 1 < len(addrs) and addrs[i + 1] < end:
            end = addrs[i + 1]
        result.append((a, end - a, name))
    return result


def named_functions(path):
    """address -> name of the `type:func` entries in a splat symbol file."""
    out = {}
    if path.exists():
        for line in path.read_text().splitlines():
            m = re.match(r"\s*([A-Za-z_]\w*)\s*=\s*0x([0-9A-Fa-f]+)\s*;(.*)", line)
            if m and "type:func" in m.group(3):
                out.setdefault(int(m.group(2), 16), m.group(1))
    return out


def overlay_slot(decomp, n):
    text = (decomp / "config" / "ovr" / f"ovr{n:02}.yaml").read_text()
    m = re.search(r"vram:\s*0x([0-9A-Fa-f]+)", text)
    if not m:
        fail(f"ovr{n:02}.yaml has no vram")
    data = (decomp / "disk" / "OVR" / f"OVR{n:02}.BIN").read_bytes()
    return int(m.group(1), 16), data


def word(data, base, addr):
    off = addr - base
    if off < 0 or off + 4 > len(data):
        return None
    return int.from_bytes(data[off:off + 4], "little")


def signatures(slots, funcs):
    """Per overlay, two (addr, word) pairs no other overlay holds at those addresses."""
    sig = {}
    for n, (base, data) in slots.items():
        found = []
        # Function entries first, then any word of the image.
        candidates = [a for a, _, _ in funcs[n]] + list(range(base, base + len(data) - 3, 4))
        for addr in candidates:
            if any(a == addr for a, _ in found):
                continue
            w = word(data, base, addr)
            if not w:
                continue
            clash = False
            for m, (b2, d2) in slots.items():
                if m != n and word(d2, b2, addr) == w:
                    clash = True
                    break
            if not clash:
                found.append((addr, w))
            if len(found) == 2:
                break
        if len(found) < 2:
            fail(f"OVR{n:02}: no two words tell it from the other overlays")
        sig[n] = found
    return sig


def symbol_addrs(decomp):
    out = {}
    for line in (decomp / "config" / "symbol_addrs.txt").read_text().splitlines():
        m = re.match(r"\s*([A-Za-z_]\w*)\s*=\s*0x([0-9A-Fa-f]+)\s*;", line)
        if m:
            out[m.group(1)] = int(m.group(2), 16)
    return out


# ---------------------------------------------------------------------------- struct probe

TYPEDEF = re.compile(r"typedef\s+struct\s*\w*\s*\{")


def typedefs(decomp):
    """Every `typedef struct {...} Name;` in the decomp's headers and sources: name -> text."""
    found = {}
    files = sorted((decomp / "include").glob("*.h")) + sorted((decomp / "src").rglob("*.c"))
    for f in files:
        text = f.read_text(errors="ignore")
        for m in TYPEDEF.finditer(text):
            depth, i = 0, m.end() - 1
            while i < len(text):
                if text[i] == "{":
                    depth += 1
                elif text[i] == "}":
                    depth -= 1
                    if depth == 0:
                        break
                i += 1
            tail = re.match(r"\}\s*(\w+)\s*;", text[i:])
            if tail and tail.group(1) not in found:
                found[tail.group(1)] = text[m.start():i + tail.end()]
    return found


def preprocess(decomp, text):
    cc = decomp / "tools" / "gcc" / "gcc-2.7.2-cdk-macos"
    flags = ["-P", "-undef", f"-I{decomp / 'include'}", "-D__GNUC__=2", "-D_LANGUAGE_C", "-DINCLUDE_ASM_USE_MACRO_INC=1"]
    with tempfile.TemporaryDirectory() as tmp:
        c = Path(tmp) / "probe.c"
        c.write_text(text)
        pre = subprocess.run([str(cc / "cpp"), *flags, str(c)], capture_output=True, text=True, cwd=decomp)
    if pre.returncode:
        fail("cpp: " + pre.stderr[-2000:])
    return pre.stdout


def probe(decomp, checks):
    """Offsets and sizes of `Type.field` for every check, measured by the decomp's cc1."""
    defs = typedefs(decomp)
    # What the headers already define (common.h pulls in types.h, psx.h and game.h).
    header = preprocess(decomp, '#include "common.h"\n')
    needed, order = set(), []

    def in_headers(t):
        return re.search(r"\}\s*" + re.escape(t) + r"\s*;", header) is not None

    def need(t):
        if t in needed or t not in defs or in_headers(t):
            return
        needed.add(t)
        for word_ in re.findall(r"\b([A-Z]\w+)\b", defs[t]):
            if word_ != t:
                need(word_)
        order.append(t)

    for t, _ in checks:
        if t not in defs:
            fail(f"no typedef {t} in the decomp")
        need(t)
    src = ['#include "common.h"']
    src += [defs[t] for t in order]
    for i, (t, f) in enumerate(checks):
        src.append(f"int lt_o_{i} = (int)&((({t} *)0)->{f});")
        src.append(f"int lt_s_{i} = sizeof((({t} *)0)->{f});")
    cc = decomp / "tools" / "gcc" / "gcc-2.7.2-cdk-macos"
    asm = subprocess.run([str(cc / "cc1"), "-quiet", "-O2", "-G0"], input=preprocess(decomp, "\n".join(src) + "\n"), capture_output=True, text=True, cwd=decomp)
    if asm.returncode:
        fail("cc1: " + asm.stderr[-2000:])
    values = dict(re.findall(r"^(lt_[os]_\d+):\s*\n\s*\.word\s+(-?\d+)", asm.stdout, re.M))
    out = []
    for i in range(len(checks)):
        out.append((int(values[f"lt_o_{i}"]), int(values[f"lt_s_{i}"])))
    return out


# ---------------------------------------------------------------------------- output


def generate(decomp):
    spec = tomllib.loads(SPEC.read_text())
    syms = symbol_addrs(decomp)
    main_map = decomp / "build" / "main" / "slus_008.11.map"
    main_named = named_functions(decomp / "config" / "symbol_addrs.txt")
    funcs = {0: functions(main_map, main_named)}
    slots = {}
    for n in OVERLAYS:
        m = decomp / "build" / f"ovr{n:02}" / f"ovr{n:02}.map"
        if not m.exists():
            fail(f"{m} is missing (build the decomp first)")
        slots[n] = overlay_slot(decomp, n)
        base, data = slots[n]
        funcs[n] = functions(m, named_functions(decomp / "config" / "ovr" / f"ovr{n:02}_symbols.txt"), (base, base + len(data)))
    sig = signatures(slots, funcs)

    # The struct checks.
    checks, where = [], []
    for sname, st in spec["struct"].items():
        for f in st["fields"]:
            if "from" in f:
                t, field = f["from"].split(".", 1)
                checks.append((t, field))
                where.append((sname, f))
    measured = probe(decomp, checks) if checks else []
    errors = []
    for (sname, f), (off, size) in zip(where, measured):
        _, want = rust_type(f["type"])
        if off != f["offset"] or size != want:
            errors.append(f"{sname}.{f['name']}: layouts.toml says +{f['offset']:#x} ({want} bytes), {f['from']} is +{off:#x} ({size} bytes)")
    for sname, st in spec["struct"].items():
        for f in st["fields"]:
            _, size = rust_type(f["type"])
            if f["offset"] + size > st["size"]:
                errors.append(f"{sname}.{f['name']} runs past the struct's size")
    if errors:
        fail("the decomp disagrees with layouts.toml:\n  " + "\n  ".join(errors))

    commit = subprocess.run(["git", "-C", str(decomp), "rev-parse", "--short=10", "HEAD"], capture_output=True, text=True).stdout.strip() or "unknown"
    o = []
    w = o.append
    w("// @generated by gen/gen.py from layouts.toml and the Guardian's Crusade decomp")
    w(f"// (gc_decomp {commit}). Don't edit: change layouts.toml, or the decomp, and run gen.py again.")
    w("")
    w("use crate::{FieldInfo, Global, Overlay, Symbol, Unit};")
    w("")
    for sname, st in spec["struct"].items():
        w(f"/// {st['doc']}")
        w("#[allow(non_snake_case)]")
        w(f"pub mod {sname} {{")
        w("    #[allow(unused_imports)]")
        w("    use crate::{Field, Matrix, SVector, Vector};")
        w("")
        w(f"    pub const SIZE: u32 = {st['size']:#x};")
        for f in st["fields"]:
            rt, _ = rust_type(f["type"])
            evidence = f"Checked against the decomp's `{f['from']}`." if "from" in f else "Not in the decomp's types; read live by `gc_sys_live`."
            doc = f.get("doc", "")
            w("")
            if doc:
                w(f"    /// {doc}")
                w("    ///")
            w(f"    /// {evidence}")
            w(f"    pub const {f['name'].upper()}: Field<{rt}> = Field::at({f['offset']:#x});")
        w("}")
        w("")
    for gname, g in spec["global"].items():
        if "symbol" in g:
            if g["symbol"] not in syms:
                fail(f"global {gname}: no symbol {g['symbol']}")
            addr, evidence = syms[g["symbol"]], f"The decomp's `{g['symbol']}`."
        else:
            addr, evidence = g["addr"], "A fixed address (no symbol in the decomp)."
        stride = g.get("stride", spec["struct"][g["type"]]["size"] if "type" in g else 0)
        count = g.get("count", 1)
        w(f"/// {g['doc']} {evidence}")
        w(f"pub const {gname}: Global = Global {{ name: \"{gname}\", addr: {addr:#010x}, stride: {stride:#x}, count: {count} }};")
        w("")
    w("/// Every struct field, for naming an address ([`crate::describe`]).")
    w("pub static FIELDS: &[FieldInfo] = &[")
    for sname, st in spec["struct"].items():
        for f in st["fields"]:
            _, size = rust_type(f["type"])
            w(f"    FieldInfo {{ structure: \"{sname}\", name: \"{f['name']}\", offset: {f['offset']:#x}, size: {size} }},")
    w("];")
    w("")
    w("/// Every global and the struct it holds, if any.")
    w("pub static GLOBALS: &[(Global, Option<&str>)] = &[")
    for gname, g in spec["global"].items():
        ty = f"Some(\"{g['type']}\")" if "type" in g else "None"
        w(f"    ({gname}, {ty}),")
    w("];")
    w("")
    main_funcs = funcs[0]
    lo, hi = min(a for a, _, _ in main_funcs), max(a + s for a, s, _ in main_funcs)
    w(f"/// Main's code: {len(main_funcs)} functions.")
    w(f"pub const MAIN_TEXT: (u32, u32) = ({lo:#010x}, {hi:#010x});")
    w("")
    w("/// The code overlays: where each loads, its size, and two words that tell it from the others.")
    w("pub static OVERLAYS: &[Overlay] = &[")
    for n in OVERLAYS:
        base, data = slots[n]
        s = ", ".join(f"({a:#010x}, {v:#010x})" for a, v in sig[n])
        w(f"    Overlay {{ n: {n}, addr: {base:#010x}, size: {len(data):#x}, signature: [{s}] }},")
    w("];")
    w("")
    total = sum(len(v) for v in funcs.values())
    w(f"/// Every function: {total} (main and the {len(slots)} overlays), by unit then address.")
    w("pub static SYMBOLS: &[Symbol] = &[")
    for n in [0, *OVERLAYS]:
        unit = "Unit::Main" if n == 0 else f"Unit::Overlay({n})"
        for a, s, name in funcs[n]:
            w(f"    Symbol {{ unit: {unit}, addr: {a:#010x}, size: {s:#x}, name: \"{name}\" }},")
    w("];")
    return "\n".join(o) + "\n"


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--decomp", default=os.environ.get("GC_DECOMP"), help="the gc_decomp checkout (or $GC_DECOMP)")
    ap.add_argument("--check", action="store_true", help="compare with src/generated.rs instead of writing it")
    a = ap.parse_args()
    if not a.decomp:
        fail("say where the decomp is: --decomp or $GC_DECOMP")
    text = generate(Path(a.decomp).resolve())
    if a.check:
        if OUT.exists() and OUT.read_text() == text:
            print("src/generated.rs is up to date")
            return
        print("src/generated.rs differs from what the decomp gives: run gen.py", file=sys.stderr)
        sys.exit(1)
    OUT.write_text(text)
    print(f"wrote {OUT.relative_to(CRATE)} ({text.count(chr(10))} lines)")


if __name__ == "__main__":
    main()
