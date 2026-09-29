"""Pure-Python ext2 formatter for LazyOS's persistent data volume.

Windows hosts have no ``mkfs.ext2``, so this package writes the (small, empty,
revision-1) volume itself. See :mod:`mkdisk.volume` for the entry points the
launchers use and ``python -m tools.mkdisk`` for the command line.
"""

from .volume import (DEFAULT_LABEL, DEFAULT_PATH, DEFAULT_SIZE, ensure_volume, format_image,
                     format_size, parse_size, status)

__all__ = ["DEFAULT_LABEL", "DEFAULT_PATH", "DEFAULT_SIZE", "ensure_volume", "format_image",
           "format_size", "parse_size", "status"]
