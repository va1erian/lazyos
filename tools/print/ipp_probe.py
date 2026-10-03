#!/usr/bin/env python3
"""Ask an IPP printer for its attributes (a tiny stand-in for
`ipptool get-printer-attributes.test`). Standard library only, works on Windows.

    python ipp_probe.py 192.168.1.42            # prints every attribute
    python ipp_probe.py 192.168.1.42 --raw out.bin   # also saves the raw reply (test fixture)
"""
import argparse
import struct
import urllib.request
from urllib.parse import urlsplit, urlunsplit

def attr(tag, name, value):
    """Encode one attribute: value tag, name, value."""
    n, v = name.encode(), value.encode()
    return struct.pack(">BH", tag, len(n)) + n + struct.pack(">H", len(v)) + v


def request(uri):
    """Build an IPP/2.0 Get-Printer-Attributes request for `uri`."""
    body = struct.pack(">BBHI", 2, 0, 0x000B, 1) + b"\x01"  # version, operation, request id, operation group
    body += attr(0x47, "attributes-charset", "utf-8")
    body += attr(0x48, "attributes-natural-language", "en")
    body += attr(0x45, "printer-uri", uri)
    body += attr(0x44, "requested-attributes", "all")
    body += attr(0x44, "", "media-col-database")  # extra value of the same attribute
    return body + b"\x03"


def value(tag, v):
    """Render one attribute value as text, by its value tag."""
    if tag in (0x21, 0x23) and len(v) == 4:  # integer, enum
        return str(struct.unpack(">i", v)[0])
    if tag == 0x22:  # boolean
        return "true" if v and v[0] else "false"
    if tag == 0x32:  # resolution
        x, y, u = struct.unpack(">iiB", v)
        return f"{x}x{y}{'dpi' if u == 3 else 'dpcm'}"
    if tag == 0x33:  # rangeOfInteger
        return "%d-%d" % struct.unpack(">ii", v)
    if tag == 0x31 and len(v) == 11:  # dateTime
        return "%04d-%02d-%02d %02d:%02d:%02d" % struct.unpack(">HBBBBB", v[:7])
    if tag == 0x30:  # octetString
        return v.hex()
    if tag in (0x10, 0x11, 0x12, 0x13):  # unsupported, unknown, no-value, not-settable
        return ""
    try:
        return v.decode()
    except UnicodeDecodeError:
        return v.hex()


def dump(data):
    """Print an IPP response as `name = value` lines, one extra value per line."""
    ver, status, _ = struct.unpack(">HHI", data[:8])
    print(f"# IPP {ver >> 8}.{ver & 255}, status 0x{status:04x}")
    i, depth, last, member = 8, 0, "", None
    while i < len(data):
        tag = data[i]
        i += 1
        if tag == 0x03:
            break
        if tag < 0x10:
            print(f"\n# group 0x{tag:02x}")
            continue
        nl = struct.unpack(">H", data[i:i + 2])[0]
        name = data[i + 2:i + 2 + nl].decode()
        i += 2 + nl
        vl = struct.unpack(">H", data[i:i + 2])[0]
        v = data[i + 2:i + 2 + vl]
        i += 2 + vl
        pad = "  " * depth
        if tag == 0x4A:  # memberAttrName: the next value belongs to this member
            member = v.decode()
            continue
        if tag == 0x37:  # endCollection
            depth -= 1
            print("  " * depth + "}")
            continue
        if tag == 0x34:  # begCollection
            if member:
                print(f"{pad}{member}: {{")
            else:
                print(f"{pad}{name or '  ' + last} = {{")
                last = name or last
            member = None
            depth += 1
            continue
        if member:
            print(f"{pad}{member}: {value(tag, v)}")
            member = None
        elif name:
            last = name
            print(f"{pad}{name} = {value(tag, v)}")
        else:
            print(f"{pad}  {value(tag, v)}")


def main():
    """Send Get-Printer-Attributes to the printer named on the command line."""
    parser = argparse.ArgumentParser(description="Ask an IPP printer for its attributes.")
    parser.add_argument("printer", help="address, host name, or full ipp:// URI")
    parser.add_argument("--raw", metavar="FILE", help="also save the raw IPP reply to FILE")
    args = parser.parse_args()
    printer = args.printer
    if "://" not in printer and printer.count(":") > 1 and not printer.startswith("["):
        printer = f"[{printer}]"  # a bare IPv6 address
    uri = printer if "://" in printer else f"ipp://{printer}/ipp/print"
    parts = urlsplit(uri)
    netloc = parts.netloc if parts.port else f"{parts.netloc}:631"  # IPP's default port
    scheme = "https" if parts.scheme == "ipps" else "http"
    url = urlunsplit((scheme, netloc, parts.path, parts.query, parts.fragment))
    req = urllib.request.Request(url, data=request(uri), headers={"Content-Type": "application/ipp"})
    data = urllib.request.urlopen(req, timeout=15).read()
    if args.raw:
        with open(args.raw, "wb") as f:
            f.write(data)
    dump(data)


if __name__ == "__main__":
    main()
