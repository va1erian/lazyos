# Archiver

Archiver opens archives like folders, extracts them, and makes new ones, in
the style of the 7-Zip File Manager. Open an archive from Files with a
double-click, drop one on the window, or start Archiver from the start menu
under **Accessories**.

## Formats

| Format | Extensions | Read | Write |
|---|---|---|---|
| ZIP (stored, deflate, zip64) | `.zip` | yes | yes |
| Tar (ustar, pax, GNU long names) | `.tar` | yes | yes |
| Gzip tarball | `.tar.gz`, `.tgz` | yes | yes |
| Zstandard tarball | `.tar.zst`, `.tzst` | yes | yes |
| xz tarball | `.tar.xz`, `.txz` | yes | no |
| One gzip / Zstandard file | `.gz`, `.zst` | yes | yes |
| One xz file | `.xz` | yes | no |
| 7-Zip (LZMA, LZMA2, Deflate, Copy) | `.7z` | yes | no |

Encrypted members are listed with a `*` after their name and are not
extracted.

## Using it

* **Browse**: double-click a folder to enter it, `..` or **Up**
  (`Backspace`) to go back. Click a column header to sort; folders always
  come first. The status bar counts the items and the selection.
* **Open** a file inside the archive with a double-click: it is extracted to
  a private temporary folder and opened with the app that handles its type.
* **Extract** (`Ctrl+E`): the selection, or the whole folder you are in, to a
  folder you name (created if missing). Existing files are replaced.
* **Test** decompresses everything and checks every checksum without
  writing anything.
* **New** (`Ctrl+N`) makes a new archive: its name's extension picks the
  format (`.zip`, `.tar.gz`, `.tar.zst`, `.tar`, `.gz`, `.zst`). **Level**
  on the toolbar chooses how hard it compresses.
* **Add** puts a file into the folder you are in; **Delete** (`Del`) removes
  the selection. The archive is rewritten beside itself and replaced only
  when that succeeded.
* Long operations show a progress bar with **Cancel**.

## Drag and drop

* Drop files and folders from Files onto Archiver to add them to the open
  archive's current folder. With no archive open, a dropped archive opens
  and anything else becomes a new archive (you choose its name).
* Drag entries out of Archiver onto a Files window to copy them there.

## Safety

An archive can name paths outside the folder it is extracted to (`../`,
absolute paths, or links that point out). Archiver never writes outside the
destination: such entries are skipped and listed when the extraction ends.
