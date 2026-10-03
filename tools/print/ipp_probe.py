#!/usr/bin/env python3
"""Ask an IPP printer for its attributes (a tiny stand-in for
`ipptool get-printer-attributes.test`). Standard library only, works on Windows.

    python ipp_probe.py 192.168.1.42            # prints every attribute
    python ipp_probe.py 192.168.1.42 --raw out.bin   # also saves the raw reply (test fixture)
"""
import struct, sys, urllib.request

def attr(tag, name, value):
    n, v = name.encode(), value.encode()
    return struct.pack(">BH", tag, len(n)) + n + struct.pack(">H", len(v)) + v

def request(uri):
    body = struct.pack(">BBHI", 2, 0, 0x000B, 1) + b"\x01"   # IPP/2.0 Get-Printer-Attributes
    body += attr(0x47, "attributes-charset", "utf-8")
    body += attr(0x48, "attributes-natural-language", "en")
    body += attr(0x45, "printer-uri", uri)
    body += attr(0x44, "requested-attributes", "all")
    body += attr(0x44, "", "media-col-database")             # extra value of the same attribute
    return body + b"\x03"

def value(tag, v):
    if tag in (0x21, 0x23) and len(v) == 4: return str(struct.unpack(">i", v)[0])
    if tag == 0x22: return "true" if v and v[0] else "false"
    if tag == 0x32: x, y, u = struct.unpack(">iiB", v); return f"{x}x{y}{'dpi' if u == 3 else 'dpcm'}"
    if tag == 0x33: return "%d-%d" % struct.unpack(">ii", v)
    if tag == 0x31 and len(v) == 11: return "%04d-%02d-%02d %02d:%02d:%02d" % struct.unpack(">HBBBBB", v[:7])
    if tag in (0x30, 0x10, 0x11, 0x12, 0x13): return v.hex() if tag == 0x30 else ""
    try: return v.decode()
    except UnicodeDecodeError: return v.hex()

def dump(data):
    ver, status, _ = struct.unpack(">HHI", data[:8])
    print(f"# IPP {ver >> 8}.{ver & 255}, status 0x{status:04x}")
    i, depth, last = 8, 0, ""
    while i < len(data):
        tag = data[i]; i += 1
        if tag == 0x03: break
        if tag < 0x10: print(f"\n# group 0x{tag:02x}"); continue
        nl = struct.unpack(">H", data[i:i + 2])[0]; name = data[i + 2:i + 2 + nl].decode(); i += 2 + nl
        vl = struct.unpack(">H", data[i:i + 2])[0]; v = data[i + 2:i + 2 + vl]; i += 2 + vl
        pad = "  " * depth
        if tag == 0x34: print(f"{pad}{name or '  ' + last} = {{"); depth += 1; last = name or last; continue
        if tag == 0x37: depth -= 1; print("  " * depth + "}"); continue
        if tag == 0x4A: print(f"{pad}{v.decode()}:", end=" "); continue
        if name: last = name; print(f"{pad}{name} = {value(tag, v)}")
        else: print(f"{pad}  {value(tag, v)}" if depth == 0 else value(tag, v))

def main():
    if len(sys.argv) < 2: sys.exit(__doc__)
    host = sys.argv[1]
    uri = host if "://" in host else f"ipp://{host}/ipp/print"
    url = uri.replace("ipps://", "https://").replace("ipp://", "http://")
    if url.count(":") < 2: url = url.replace("/ipp/", ":631/ipp/", 1)
    req = urllib.request.Request(url, data=request(uri), headers={"Content-Type": "application/ipp"})
    data = urllib.request.urlopen(req, timeout=15).read()
    if "--raw" in sys.argv: open(sys.argv[sys.argv.index("--raw") + 1], "wb").write(data)
    dump(data)

if __name__ == "__main__":
    main()
