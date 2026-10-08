#!/usr/bin/env python3
"""Remove a host path from DT_NEEDED when linking an older no-SONAME quill.

SDK builds of quill/build.sh set SONAME and do not need this. Older Move display
libraries linked by Zig can produce an absolute DT_NEEDED; patch only that entry.
"""
import pathlib
import struct
import sys

path = pathlib.Path(sys.argv[1])
data = bytearray(path.read_bytes())
if data[:6] != b"\x7fELF\x02\x01":
    raise SystemExit("Expected a little-endian ELF64 binary")
phoff = struct.unpack_from("<Q", data, 32)[0]
phsize, phcount = struct.unpack_from("<HH", data, 54)
loads = []
dynamic = None
for index in range(phcount):
    values = struct.unpack_from("<IIQQQQQQ", data, phoff + index * phsize)
    kind, _, offset, address, _, size, _, _ = values
    if kind == 1:
        loads.append((address, offset, size))
    elif kind == 2:
        dynamic = (offset, size)
if dynamic is None:
    raise SystemExit("No dynamic section")
entries = []
for offset in range(dynamic[0], sum(dynamic), 16):
    tag, value = struct.unpack_from("<qQ", data, offset)
    if tag == 0:
        break
    entries.append((tag, value))
address = next(value for tag, value in entries if tag == 5)
string_offset = next(offset + address - start for start, offset, size in loads
                     if start <= address < start + size)
for tag, value in entries:
    if tag != 1:
        continue
    start = string_offset + value
    end = data.index(0, start)
    name = bytes(data[start:end])
    if name.endswith(b"/libquill.so"):
        replacement = b"libquill.so"
        data[start:end] = replacement + b"\0" * (len(name) - len(replacement))
path.write_bytes(data)
