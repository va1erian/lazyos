# LazyWriter printing to an HP DeskJet 3700 — exploration and plan (blue sky)

> **Status: exploratory, revision 1 (2026-10-03). Nothing here is built.**
> This answers one question: how could LazyWriter on LazyOS print to an HP
> DeskJet 3700? Short answer: send each page as PWG Raster over plain IPP to
> the printer's network address. The printer was probed on 2026-10-03 (P0
> below); its full reply is
> [`printing/deskjet3700-attributes.txt`](printing/deskjet3700-attributes.txt)
> (Wi-Fi name, serial number, MAC suffix and UUID redacted). Facts not checked
> against that reply or the source tree are marked *unverified*.

Related: [xui-writer.md](xui-writer.md) (LazyWriter, its page view),
[networking-plan.md](networking-plan.md) and
[architecture/networking.md](architecture/networking.md) (`netd`, `TcpStream`),
[networking-host-access.md](networking-host-access.md) (reaching the LAN from
QEMU), [architecture/usb.md](architecture/usb.md) and
[architecture/usb-storage.md](architecture/usb-storage.md) (the USB fallback),
[tls-plan.md](tls-plan.md), xui's `docs/plans/page-view.md`.

## 1. Short answer

1. **Use IPP over the network.** The DeskJet 3700 is an AirPrint printer. It
   accepts IPP/2.0 on `ipp://<printer>/ipp/print` (port 631, no TLS required)
   and takes `image/pwg-raster` in `srgb_8` or `sgray_8` at 300 dpi, the
   simplest raster format there is. URF (Apple Raster) and PCLm are accepted
   too, as backups.
2. **No new driver, no kernel change.** LazyOS already has TCP sockets for
   apps (`netd`, `TcpStream`), Net Tools already speaks HTTP, and from QEMU the
   guest reaches the printer's LAN address through slirp's NAT. xui can already
   draw offscreen into an RGBA image (`xui-canvas` `OffscreenBackend`), and
   LazyWriter now has pages (page view, #548).
3. **New work is four small pieces**, all host-testable: an IPP message codec
   (`libs/ipp`), a raster encoder (`libs/raster`), rendering a page at 300 dpi
   in bands, and a print dialog. A `printd` spooler is optional and later.
4. **USB is the fallback, not the plan.** The printer offers no IPP-over-USB
   (no 7/1/4 interface). Its 7/1/2 printer interface would need a new class
   driver in `usbd`, though its IEEE 1284 device ID suggests it takes the same
   PWG Raster there (*unverified*).

## 2. What the printer offers (probed 2026-10-03)

Probed with [`tools/print/ipp_probe.py`](../tools/print/ipp_probe.py) (a
standard-library stand-in for `ipptool get-printer-attributes`) against the
printer at `192.168.1.89`. Firmware `LYP2FN2218AR`.

| Attribute | Value | What it means for us |
|---|---|---|
| `printer-uri-supported` | `ipp://…/ipp/print`, `ipps://…:443/ipp/print` | Plain IPP works; `uri-security-supported` includes `none`, so no TLS needed |
| `ipp-versions-supported` | 1.0, 1.1, 2.0 | Speak 2.0 |
| `document-format-supported` | `application/vnd.hp-PCL`, `image/jpeg`, `application/PCLm`, `image/urf`, `image/pwg-raster`, `application/octet-stream` | Send `image/pwg-raster` |
| `pwg-raster-document-type-supported` | `sgray_8`, `srgb_8`, `adobe-rgb_8`, `rgb_8` and the 16-bit variants | `srgb_8` for colour, `sgray_8` for grey |
| `pwg-raster-document-resolution-supported` | 300x300 dpi only | Render at 300 dpi: A4 is 2480 x 3508 pixels |
| `pwg-raster-document-sheet-back` | `rotated` | Irrelevant: one-sided only |
| `urf-supported` | `CP1 MT1-2-8-9-10-11 PQ3-4-5 RS300 SRGB24 OB9 OFU0 W8-16 …` | URF at 300 dpi, sRGB 24-bit or grey 8-bit, as a backup |
| `compression-supported` | `none`, `deflate`, `gzip` | Start with none; the raster's own line compression is enough |
| Margins (`media-*-margin-supported`) | left, right, top 2.96 mm; bottom 12.7 mm | LazyWriter's default margins (25 mm, 1 in) are well clear |
| `media-default` | `iso_a4_210x297mm`; Letter, Legal, A5, A6, B5, envelopes and photo sizes also listed | A4 and Letter cover page view's paper choices |
| `orientation-requested-supported` | 3 (portrait) only | Landscape pages are rotated by us before encoding |
| `sides-supported` | `one-sided` | No duplex in the dialog |
| `print-quality-supported` | 3, 4, 5 (draft, normal, high) | Maps straight to a quality picker |
| `print-color-mode-supported` | `auto`, `auto-monochrome`, `monochrome`, `color`, `process-monochrome` | Colour or grey picker |
| `copies-supported`, `page-ranges-supported` | 1–99, true | The printer handles copies and ranges itself |
| `operations-supported` | Print-Job, Validate-Job, Create-Job, Send-Document, Cancel-Job, Get-Job-Attributes, Get-Jobs, Get-Printer-Attributes, Identify-Printer and more | Everything P4 needs |
| `multiple-document-jobs-supported` | false | One document per job |
| `jpeg-k-octets-supported` | 0–12288 | JPEG pages up to 12 MiB (not planned) |
| `marker-names`, `marker-levels` | tri-color ink 90 %, black ink 50 % | The dialog can show ink levels for free |
| USB interfaces (`printer-device-id`, `LEDMDIS`) | `FF/CC/00`, `07/01/02`, `FF/04/01` | No IPP-over-USB; 7/1/2 is the bidirectional printer class |
| Languages (`printer-device-id`, `CMD`) | `PCL3GUI, PJL, Automatic, JPEG, PCLM, AppleRaster, PWGRaster, DW-PCL` | The USB channel likely takes PWG Raster too (*unverified*) |

Other facts: Wi-Fi 802.11b/g/n at 2.4 GHz only, Wi-Fi Direct, USB 2.0
Hi-Speed, no Ethernet ([HP user guide](https://www.adorama.com/col/productManuals/IHPJ9V92A.pdf)).
On Linux, HPLIP drives it as `hp:/usb/…` with `hpcups` (PCL3GUI) and scans
over eSCL ([HPLIP question 696166](https://answers.launchpad.net/hplip/+question/696166)).

## 3. What LazyOS already has

Checked against `main` at `aabf64d` (lazyos) and `c2552af` (xui).

| Piece | State | Where |
|---|---|---|
| TCP sockets for apps | Built (N3): `TcpStream` over `os.lazy.net.socket.v1`; `AF_INET` in the Linux shim (N5) | `user/src/messenger/netstd.rs`, [architecture/networking.md](architecture/networking.md) |
| An HTTP client in an app | Built: Net Tools fetches pages over `std::net` on worker threads | `xui-app/src/bin/nettools.rs` |
| Reaching a LAN host from QEMU | Works: slirp NATs outbound TCP to any address the host reaches | [networking-host-access.md](networking-host-access.md) |
| Name lookups | Built, unicast DNS only | `user/src/bin/netd/resolve.rs` |
| mDNS / DNS-SD discovery | Missing: no multicast join in `netstack`, and slirp drops multicast | — |
| Offscreen rendering to RGBA | Built: `OffscreenBackend`, `RgbaImage` on tiny-skia | xui `crates/xui-canvas` |
| Pages | Built (#548): `PageSetup` in dip (96 per inch), A4/Letter, portrait/landscape, margins, page breaks; zoom is a layout DPI | xui `crates/xui-rich-text/src/model/page.rs`, `layout/pages.rs`; [xui-writer.md](xui-writer.md) |
| USB | `usbd`: xHCI, hubs, HID, mass storage with bulk endpoints; no printer class | `user/src/bin/usbd/class.rs`, `msc.rs` |
| TLS | Exploratory plan, not built (not needed here) | [tls-plan.md](tls-plan.md) |

## 4. Routes compared

| Route | Transport | Page format | New LazyOS work | Verdict |
|---|---|---|---|---|
| A. IPP over the LAN | HTTP/1.1 on TCP 631 | PWG Raster (URF as backup) | IPP codec, raster encoder, page render, dialog | **Recommended** |
| B. Raw port 9100 | Plain TCP | Whatever the firmware sniffs | As A minus IPP, but no status or errors back | Not needed; port not probed |
| C. IPP-over-USB | USB class 7/1/4 | PWG Raster | — | **Ruled out**: the printer has no 7/1/4 interface |
| D. USB printer class | USB class 7/1/2, bulk out | PWG Raster if the device ID is right, else PCL3GUI | A printer binding in `usbd/class.rs` (bulk pipes exist for mass storage), `GET_DEVICE_ID`, a block-free print path; then B's blind send | Fallback for a printer with no network |

Route A also works from QEMU today without USB passthrough, because the
guest's TCP connection leaves through the host.

## 5. Proposed design

```
LazyWriter process (P2–P5)
  paginated document --> band renderer --> raster encoder --> IPP job writer
  (page view, #548)      OffscreenBackend   libs/raster         libs/ipp, HTTP POST,
                         300 dpi bands      PWG srgb_8/sgray_8  Get-Job-Attributes
                                                                     |
                                                     TcpStream to printer:631
                                                                     v
  DeskJet 3700 <-- home LAN <-- host NAT (QEMU slirp) <-- netdrv <-- netd
```

* **Rendering.** Page view already lays text out at `dpi * zoom`, so a page is
  laid out at 300 dpi (zoom 300/96) and painted with `OffscreenBackend` one
  band at a time, translated per band. A whole A4 page in RGBA at 300 dpi is
  2480 x 3508 x 4 bytes, about 35 MB; 256-row bands are about 2.5 MB, so
  memory stays flat whatever the page count. Landscape pages are rotated 90°
  per band, since the printer only accepts portrait. Content is printed dark on
  white, as page view already draws it.
* **Encoding.** PWG Raster (PWG 5102.4): a `RaS2` sync word, then per page a
  1796-byte header (size, resolution, colour space, bits per pixel) and the
  pixels with PackBits-style line compression and line repeat. It streams row
  by row, so bands go straight out.
* **Submission.** One `Print-Job` per document (`multiple-document-jobs` is
  false): an HTTP/1.1 chunked `POST` of `application/ipp` with
  `document-format=image/pwg-raster`, `media`, `print-color-mode`,
  `print-quality`, `copies`. Then `Get-Job-Attributes` until `job-state` is
  completed, aborted or canceled, surfacing `job-state-reasons` and the
  printer's `printer-state-message` as the dialog's status line.
* **Where it runs.** First on a worker thread inside LazyWriter, using the
  network access LazyWriter would need anyway. In P6 the encoder and writer
  move into `printd`, LazyWriter hands it pages through a Messenger shared
  buffer, and only `printd` needs network access for printing.

## 6. Staged plan

1. **P0, probe the printer.** *Done 2026-10-03*: §2 and the fixture. Re-run
   `python tools/print/ipp_probe.py <ip> --raw out.bin` against any other
   printer. (On Windows, PowerShell's `>` writes UTF-16; the checked-in text
   copy is UTF-8.)
2. **P1, `libs/ipp`.** `no_std` encoder and decoder for IPP/2.0 messages
   (RFC 8010): attribute groups, the value tags we use, collections. Host tests
   decode the P0 fixture (a raw `--raw` capture to be added alongside it) and
   round-trip requests; a fuzz target like `libs/netstack`'s.
3. **P2, `libs/raster`.** PWG Raster encoder streaming bands, `srgb_8` and
   `sgray_8`; URF behind the same trait as the backup. Host tests decode the
   output back and compare pixels; a fuzz target for the decoder used in tests.
4. **P3, render pages.** A `print_pages(doc, setup, dpi) -> impl Iterator<Band>`
   on top of xui-rich-text's page layout and `OffscreenBackend`; host test that
   a 300 dpi render of a known `.lzw` matches a reference within a tolerance.
5. **P4, submit a job.** `Validate-Job`, then `Print-Job` over `TcpStream`,
   then `Get-Job-Attributes` polling and `Cancel-Job`. On a worker thread in
   LazyWriter.
6. **P5, print dialog.** `Ctrl+P` and a toolbar Print button: printer address
   (remembered in `confd`), copies, page range, colour or grey, draft / normal
   / high; paper and orientation come from Page setup. A status line with the
   printer's own words and ink levels from `marker-levels`.
7. **P6, `printd` (optional).** A spooler serving `os.lazy.print.v1` (new MIDL),
   so other apps print and a job outlives LazyWriter.

Later, outside this plan: DNS-SD discovery (needs multicast in `netstack` and a
bridged network in QEMU), route D for USB-only printers, PCLm for printers
without PWG Raster.

## 7. Testing without a printer

CI never sees the DeskJet, so the verdict is what a fake printer on the host
received, as `tools/net/run.py` judges the packet capture.

* **Fake printer.** `ippeveprinter` (CUPS / libcups3, or PAPPL) runs an IPP
  Everywhere printer on the host and saves each job to a file. Started with the
  P0 attributes, it imitates the DeskJet's capabilities.
* **Harness.** A `tools/print/run.py` boots LazyOS with `--net`, has LazyWriter
  print a known `.lzw` to `10.0.2.2:<port>`, then decodes the saved raster
  and compares it with a host-side reference render of the same document.
* **Real printer.** A manual checklist per stage from a desktop on the same
  Wi-Fi: one page, five pages, A4 and Letter, landscape, grey, out of paper,
  cancel mid-job.

## 8. Risks and open questions

* **Addressing.** Users type an IP or a DNS name; the printer's DHCP address
  can change. A reserved address on the router avoids surprises until
  DNS-SD exists.
* **Colour.** sRGB in, the printer's own colour handling out. Good enough for
  documents; no ICC work planned.
* **Speed.** 300 dpi `srgb_8` A4 is about 26 MB uncompressed per page before
  line compression; mostly-white document pages compress well. To measure in
  P4 over slirp.
* **Route D's language.** Whether the USB channel accepts PWG Raster without a
  PJL wrapper is *unverified*; PCL3GUI is HP-specific, and HPLIP's encoder
  licence would need checking against GPL-3.0-or-later before any of it is
  read for porting.
