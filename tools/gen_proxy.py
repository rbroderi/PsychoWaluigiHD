"""Generate the counting mmfs2.dll proxy sources from the real DLL + SDK ordinal map.

Writes into proxy/mmfs2_proxy:
  exports.def       - every ordinal; functions -> thunks, data -> copied tables
  src/thunks.rs     - one asm thunk per function ordinal: count, then jump to the real export
  src/names.rs      - ordinal -> decorated name table for the log

Usage: python tools/gen_proxy.py
"""
import os
import struct
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "tools"))
import pe_dump  # noqa: E402

ORIG = os.path.join(ROOT, "runtime_orig", "mmfs2.dll")
ORDINALS = os.path.join(ROOT, "tools", "mmfs2_ordinals.tsv")
OUT = os.path.join(ROOT, "proxy", "mmfs2_proxy")

# Exports routed to typed Rust hooks (hook_<key> in src/hooks.rs) instead of counting thunks.
HOOKS = {n: f"_{n}@{size}" for n, size in [
    ("AddSprite", 36), ("AddOwnerDrawSprite", 44), ("ModifSprite", 20), ("ModifSpriteEx", 40),
    ("ModifSpriteEffect", 16), ("ModifOwnerDrawSprite", 24), ("ShowSprite", 12),
    ("DelSprite", 8), ("DelSpriteFast", 8), ("DelSprites", 4),
    ("MoveSpriteToFront", 8), ("MoveSpriteToBack", 8), ("MoveSpriteBefore", 12),
    ("MoveSpriteAfter", 12), ("SwapSprites", 12), ("SetSpriteLayer", 12),
    ("PasteSprite", 20), ("PasteSpriteEffect", 28), ("AddImage", 44), ("DelImage", 8),
    ("WinGetLogRect", 8), ("SpriteDraw", 4), ("WinScroll", 28),
]}
HOOKS.update({
    "LockImageSurface": "?LockImageSurface@@YGHPAXKAAVcSurface@@H@Z",
    "UnlockImageSurface": "?UnlockImageSurface@@YGXAAVcSurface@@@Z",
    # cSurface::Blit(dest, destX, destY, srcX, srcY, srcW, srcH, BlitMode, BlitOp, LPARAM, flags)
    "SurfBlitRect": "?Blit@cSurface@@QBEHAAV1@HHHHHHW4BlitMode@@W4BlitOp@@JK@Z",
    # cSurface::Blit(dest, destX, destY, BlitMode, BlitOp, LPARAM, flags) and Blit(dest)
    "SurfBlitAt": "?Blit@cSurface@@QBEHAAV1@HHW4BlitMode@@W4BlitOp@@JK@Z",
    "SurfBlitAll": "?Blit@cSurface@@QBEHAAV1@@Z",
    # Surface lifetime, to forget what an off-screen surface held.
    "SurfCreate": "?Create@cSurface@@QAEXHHPAV1@@Z",
    "SurfFillColor": "?Fill@cSurface@@QAEHK@Z",
    "DeleteSurface": "?DeleteSurface@@YGXPAVcSurface@@@Z",
    # New levels are rendered off-screen and then attached as the window's screen surface.
    "WinAttachSurface": "?WinAttachSurface@@YGHHPAVcSurface@@@Z",
    # cSurface::Rectangle(l, t, r, b, CFillData* fill, BOOL antiA, BlitMode, BlitOp, LPARAM)
    "SurfRectFill": "?Rectangle@cSurface@@QAEHHHHHPAVCFillData@@HW4BlitMode@@W4BlitOp@@J@Z",
    # cSurface::Rectangle(l, t, r, b, CFillData* fill, int thickness, CFillData* outline, BOOL antiA,
    #                     BlitMode, BlitOp, LPARAM, BOOL)
    "SurfRectFillOutline": "?Rectangle@cSurface@@QAEHHHHHPAVCFillData@@H0HW4BlitMode@@W4BlitOp@@JH@Z",
})
# Exports the hooks call but don't wrap.
CALLED = {
    "GetFirstSprite": "_GetFirstSprite@12",
    "GetNextSprite": "_GetNextSprite@12",
    "WinGetSurface": "?WinGetSurface@@YGPAVcSurface@@HH@Z",
    "SurfLockBuffer": "?LockBuffer@cSurface@@QAEPAEXZ",
    "SurfUnlockBuffer": "?UnlockBuffer@cSurface@@QAEXPAE@Z",
    "SurfGetPitch": "?GetPitch@cSurface@@QBEHXZ",
    "SurfGetWidth": "?GetWidth@cSurface@@QBEHXZ",
    "SurfGetHeight": "?GetHeight@cSurface@@QBEHXZ",
    "SurfGetDepth": "?GetDepth@cSurface@@QBEHXZ",
    "VtblFillMosaic": "??_7CFillMosaic@@6B@",
    "VtblFillDir": "??_7CFillDir@@6B@",
    "VtblFillFlat": "??_7CFillFlat@@6B@",
}


def export_table():
    d, off, _cstr, dirs, _base = pe_dump.load(ORIG)
    pe = struct.unpack_from("<I", d, 0x3C)[0]
    nsec = struct.unpack_from("<H", d, pe + 6)[0]
    sec = pe + 24 + struct.unpack_from("<H", d, pe + 20)[0]
    text = None
    for i in range(nsec):
        if d[sec + i * 40:sec + i * 40 + 8].rstrip(b"\0") == b".text":
            vs, va = struct.unpack_from("<II", d, sec + i * 40 + 8)
            text = (va, va + vs)
    f = struct.unpack_from("<IIHHIIIIIII", d, off(dirs[0][0]))
    obase, nfunc, afunc = f[5], f[6], f[8]
    out = []
    for i in range(nfunc):
        rva = struct.unpack_from("<I", d, off(afunc) + 4 * i)[0]
        if rva:
            out.append((obase + i, text[0] <= rva < text[1]))
    return out


def main():
    names = {}
    for line in open(ORDINALS):
        o, _kind, name = line.rstrip("\n").split("\t")
        names[int(o)] = name
    exports = export_table()
    max_ord = max(o for o, _ in exports)

    by_name = {n: o for o, n in names.items()}
    hooked = {by_name[n]: short for short, n in HOOKS.items()}

    os.makedirs(os.path.join(OUT, "src"), exist_ok=True)
    with open(os.path.join(OUT, "exports.def"), "w") as f:
        f.write("LIBRARY MMFS2\nEXPORTS\n")
        for o, is_code in exports:
            if o in hooked:
                f.write(f"    hook_{hooked[o]} @{o} NONAME\n")
            elif is_code:
                f.write(f"    thunk_{o} @{o} NONAME\n")
            else:
                f.write(f"    data_{o} @{o} NONAME DATA\n")

    code = [o for o, c in exports if c]
    data = [o for o, c in exports if not c]
    with open(os.path.join(OUT, "src", "thunks.rs"), "w") as f:
        f.write("// Generated by tools/gen_proxy.py - do not edit.\n")
        f.write(f"pub const MAX_ORDINAL: usize = {max_ord};\n")
        f.write(f"pub static CODE_ORDINALS: [u16; {len(code)}] = {code!r};\n")
        f.write("\n/// Ordinals of the functions the hooks wrap or call.\n#[allow(non_upper_case_globals)]\npub mod ord {\n")
        for short, n in sorted({**HOOKS, **CALLED}.items()):
            f.write(f"    pub const {short}: usize = {by_name[n]};\n")
        f.write("}\n")
        # Data exports (vtables, statics) can't be thunked; each is a copy of the original's
        # first DATA_WORDS dwords, filled in at load time before any importer runs.
        f.write("pub const DATA_WORDS: usize = 64;\n")
        for o in data:
            f.write(f"#[no_mangle]\npub static mut data_{o}: [usize; DATA_WORDS] = [0; DATA_WORDS];\n")
        entries = ", ".join(f"({o}, core::ptr::addr_of_mut!(data_{o}))" for o in data)
        f.write(f"pub fn data_exports() -> [(u16, *mut [usize; DATA_WORDS]); {len(data)}] {{\n"
                f"    [{entries}]\n}}\n\n")
        f.write("core::arch::global_asm!(\n    \".text\",\n")
        for o in code:
            f.write(f"    \".globl _thunk_{o}\",\n    \"_thunk_{o}:\",\n"
                    f"    \"lock inc dword ptr [{{counts}} + {4 * o}]\",\n"
                    f"    \"jmp dword ptr [{{real}} + {4 * o}]\",\n")
        f.write("    counts = sym crate::COUNTS,\n    real = sym crate::REAL,\n);\n")

    with open(os.path.join(OUT, "src", "names.rs"), "w") as f:
        f.write("// Generated by tools/gen_proxy.py - do not edit.\n")
        f.write("pub fn name(ordinal: usize) -> &'static str {\n    match ordinal {\n")
        for o in sorted(names):
            f.write(f"        {o} => {names[o]!r},\n".replace("'", '"'))
        f.write("        _ => \"?\",\n    }\n}\n")
    print(f"{len(code)} thunks, {len(exports) - len(code)} copied data exports, max ordinal {max_ord}")


if __name__ == "__main__":
    main()
