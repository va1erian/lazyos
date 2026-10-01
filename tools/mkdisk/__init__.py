"""Pure-Python ext2 formatter for LazyOS's persistent data volume.

Windows hosts have no ``mkfs.ext2``, so this package writes the (small,
revision-1) volume itself, with a seeded directory layout (user homes and a
sticky ``/tmp``, see :mod:`mkdisk.layout`). :mod:`mkdisk.volume` holds the entry
points the launchers use; ``python -m tools.mkdisk`` is the command line.
"""

from .layout import EMPTY, DirSpec, Layout, describe, home_volume, seeded
from .volume import (DEFAULT_HOME_PATH, DEFAULT_LABEL, DEFAULT_PATH, DEFAULT_SIZE, HOME_LABEL,
                     ensure_volume, format_image, format_size, parse_size, status)

__all__ = ["DEFAULT_HOME_PATH", "DEFAULT_LABEL", "DEFAULT_PATH", "DEFAULT_SIZE", "EMPTY",
           "HOME_LABEL", "DirSpec", "Layout", "describe", "ensure_volume", "format_image",
           "format_size", "home_volume", "parse_size", "seeded", "status"]
