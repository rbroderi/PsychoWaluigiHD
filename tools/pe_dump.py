"""Minimal PE32 import/export lister: python pe_dump.py <file> [exports|imports]"""
import struct
import sys


def load(path):
    d = open(path, "rb").read()
    pe = struct.unpack_from("<I", d, 0x3C)[0]
    nsec = struct.unpack_from("<H", d, pe + 6)[0]
    opt = pe + 24
    magic = struct.unpack_from("<H", d, opt)[0]
    dd = opt + (96 if magic == 0x10B else 112)
    secs = []
    sec = opt + struct.unpack_from("<H", d, pe + 20)[0]
    for i in range(nsec):
        vs, va, rs, ro = struct.unpack_from("<IIII", d, sec + i * 40 + 8)
        secs.append((va, max(vs, rs), ro))
    base = struct.unpack_from("<I", d, opt + 28)[0]

    def off(rva):
        for va, sz, ro in secs:
            if va <= rva < va + sz:
                return rva - va + ro
        raise ValueError(hex(rva))

    def cstr(rva):
        o = off(rva)
        return d[o:d.index(b"\0", o)].decode("latin1")

    dirs = [struct.unpack_from("<II", d, dd + i * 8) for i in range(16)]
    return d, off, cstr, dirs, base


def exports(path):
    d, off, cstr, dirs, base = load(path)
    rva, _ = dirs[0]
    o = off(rva)
    (_, _, _, _, _name, obase, nfunc, nnames, afunc, anames, aords) = struct.unpack_from("<IIHHIIIIIII", d, o)
    out = []
    for i in range(nnames):
        name = cstr(struct.unpack_from("<I", d, off(anames) + 4 * i)[0])
        ordi = struct.unpack_from("<H", d, off(aords) + 2 * i)[0]
        frva = struct.unpack_from("<I", d, off(afunc) + 4 * ordi)[0]
        out.append((ordi + obase, base + frva, name))
    return out


def imports(path):
    d, off, cstr, dirs, base = load(path)
    rva, _ = dirs[1]
    o = off(rva)
    out = []
    while True:
        oft, _, _, name, ft = struct.unpack_from("<IIIII", d, o)
        if not name:
            break
        dll = cstr(name)
        t = off(oft or ft)
        j = 0
        while True:
            e = struct.unpack_from("<I", d, t + 4 * j)[0]
            if not e:
                break
            fn = f"#{e & 0xFFFF}" if e & 0x80000000 else cstr(e + 2)
            out.append((dll, base + ft + 4 * j, fn))
            j += 1
        o += 20
    return out


if __name__ == "__main__":
    mode = sys.argv[2] if len(sys.argv) > 2 else "exports"
    rows = exports(sys.argv[1]) if mode == "exports" else imports(sys.argv[1])
    for r in rows:
        print(*[hex(x) if isinstance(x, int) and x > 0xFFFF else x for x in r], sep="\t")
