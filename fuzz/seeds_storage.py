"""Seeds for the storage and firmware fuzz targets (`gen_corpus.py`): ext2fs
scripts, ACPI golden dumps, USB mass storage and NVMe.
"""
import struct
from pathlib import Path

# ---- ext2fs script grammar ----------------------------------------------------
# `libs/ext2fs/src/fuzz.rs`: a head byte (bit 0 picks corruption mode, bit 1 the
# block size: clear 1 KiB, set 4 KiB), then either four-byte operations
# `kind a b c` (model mode) or three-byte `hi lo value` byte overwrites of the
# image's first 48 KiB (corruption mode).

MKDIR, CREATE, WRITE, TRUNCATE, UNLINK, RMDIR, RENAME, READ = range(8)


def e2_op(kind, a=0, b=0, c=0):
    return bytes([kind, a, b, c])


def e2_poke(at, value):
    return bytes([at >> 8, at & 0xFF, value])


def ext2fs_seeds():
    # a=1 names /d0, a=2 /d1, a=0 the root; b picks the file (b % 4) or the directory (b % 2).
    tree = e2_op(MKDIR, 0, 0) + e2_op(MKDIR, 0, 1)
    workout = (
        tree
        + e2_op(CREATE, 1, 0) + e2_op(CREATE, 1, 1) + e2_op(CREATE, 0, 2)
        + e2_op(WRITE, 1, 0, 40) + e2_op(WRITE, 1, 1, 200) + e2_op(WRITE, 0, 2, 255)
        + e2_op(READ, 1, 0) + e2_op(TRUNCATE, 1, 0, 3) + e2_op(WRITE, 1, 100, 80)
        + e2_op(RENAME, 1, 1, 2) + e2_op(READ, 2, 1) + e2_op(UNLINK, 0, 2)
        + e2_op(RMDIR, 0, 1) + e2_op(UNLINK, 2, 1) + e2_op(RMDIR, 0, 1)
    )
    # 1 KiB blocks: the second file crosses into the single-indirect range.
    indirect = tree + e2_op(CREATE, 1, 0) + e2_op(WRITE, 1, 0, 250) + e2_op(WRITE, 1, 200, 250) + e2_op(READ, 1, 0)
    churn = tree + b"".join(
        e2_op(CREATE, 1 + i % 2, i) + e2_op(WRITE, 1 + i % 2, i, 60 + i) + e2_op(UNLINK, 1 + i % 2, i)
        for i in range(24)
    )
    errors = (
        e2_op(CREATE, 1, 0)  # no /d0 yet
        + e2_op(UNLINK, 0, 0) + e2_op(RMDIR, 0, 0) + e2_op(READ, 0, 0) + e2_op(TRUNCATE, 0, 0, 5)
        + tree + e2_op(MKDIR, 0, 0) + e2_op(RENAME, 1, 0, 0) + e2_op(RENAME, 1, 0, 1)
    )
    sb = 1024  # superblock byte offset; descriptors follow in block 2 (1 KiB blocks)
    gdt = 2048
    return {
        "model_workout_1k": bytes([0]) + workout,
        "model_workout_4k": bytes([2]) + workout,
        "model_indirect_1k": bytes([0]) + indirect,
        "model_indirect_4k": bytes([2]) + indirect,
        "model_churn": bytes([0]) + churn,
        "model_errors": bytes([0]) + errors,
        "corrupt_magic": bytes([1]) + e2_poke(sb + 0x38, 0),
        "corrupt_log_block_size": bytes([1]) + e2_poke(sb + 0x18, 3),
        "corrupt_counts": bytes([1]) + e2_poke(sb + 0x07, 0x7F) + e2_poke(sb + 0x0F, 0xFF),
        # Groups with more bits than one bitmap block holds (must be refused at mount).
        "corrupt_blocks_per_group": bytes([1]) + e2_poke(sb + 0x21, 0xFF) + e2_poke(sb + 0x22, 0x01),
        "corrupt_inodes_per_group": bytes([1]) + e2_poke(sb + 0x29, 0x80),
        "corrupt_incompat": bytes([1]) + e2_poke(sb + 0x60, 0x42),
        "corrupt_inode_table": bytes([1]) + e2_poke(gdt + 8, 0xFF) + e2_poke(gdt + 11, 0x7F),
        "corrupt_bitmaps": bytes([1]) + e2_poke(gdt + 0, 0x01) + e2_poke(gdt + 4, 0x01),
        "corrupt_dir_records": bytes([1]) + e2_poke(2048 + 32 * 4 + 4, 0) + e2_poke(2048 + 32 * 4 + 6, 0xC8),
        "corrupt_none": bytes([1]),
        "empty": b"",
    }


# ---- acpi: golden firmware dumps as physical-memory images ---------------------

ACPI_GOLDEN = Path(__file__).resolve().parent.parent / "libs" / "acpi" / "golden"


def acpi_seeds():
    """`libs/acpi/src/fuzz.rs` input: a mode byte (1 = re-seal checksums) and
    the body of a golden dump (`tools/acpi/dump_tables.py`) without its magic."""
    seeds = {}
    for dump in sorted(ACPI_GOLDEN.glob("*.bin")):
        body = dump.read_bytes()[8:]
        seeds[dump.stem] = bytes([0]) + body
        seeds[dump.stem + "_sealed"] = bytes([1]) + body
    seeds["empty"] = b""
    seeds["rsdp_only"] = bytes([1]) + struct.pack("<Q", 0x1000)
    return seeds


# ---- usbmsc (mass storage) ------------------------------------------------------

MSC_HS_CONFIG = bytes.fromhex(
    "090220000101" "00c032"
    "090400000208065000"
    "07058102000200"
    "07050202000200"
)
MSC_SS_CONFIG = bytes.fromhex(
    "09022c000101" "00c032"
    "090400000208065000"
    "07058102000400" "06300f000000"
    "07050202000400" "063003000000"
)


def mscdesc_seeds():
    composite = bytes.fromhex(
        "090239000201" "00a032"
        "090400000103010100" "092111010001223f00" "07058103080007"
        "090401000208065000" "07058302400000" "07050402400000"
    )
    return {
        "hs_stick": MSC_HS_CONFIG,
        "ss_stick": MSC_SS_CONFIG,
        "composite": composite,
        "alt_setting": MSC_HS_CONFIG[:12] + bytes([1]) + MSC_HS_CONFIG[13:],
        "zero_blength": MSC_HS_CONFIG[:18] + bytes([0]) + MSC_HS_CONFIG[19:],
        "orphan_companion": MSC_HS_CONFIG[:9] + bytes.fromhex("06300f000000") + MSC_HS_CONFIG[9:],
        "empty": b"",
    }


def mscreply_seeds():
    csw = b"USBS" + struct.pack("<II", 7, 0) + bytes([0])
    sense = bytes([0x70, 0, 0x06]) + bytes(9) + bytes([0x28, 0]) + bytes(4)
    return {
        "csw_good": bytes([7]) + csw,
        "csw_wrong_tag": bytes([8]) + csw,
        "csw_phase": bytes([7]) + csw[:12] + bytes([2]),
        "inquiry": bytes([0]) + bytes([0, 0x80, 5, 2, 31, 0, 0, 0]) + b"LAZYOS  MODEL STICK     1.00",
        "sense_fixed": bytes([0]) + sense,
        "sense_descriptor": bytes([0, 0x72, 0x02, 0x3A, 0x00]),
        "capacity10": bytes([0]) + struct.pack(">II", 0x3FFFFF, 512),
        "capacity10_huge": bytes([0]) + struct.pack(">II", 0xFFFFFFFF, 512),
        "capacity16": bytes([0]) + struct.pack(">QI", (1 << 40), 4096) + bytes(20),
        "capacity16_overflow": bytes([0]) + struct.pack(">QI", (1 << 64) - 1, 512) + bytes(20),
        "mode_sense_wp": bytes([0, 3, 0, 0x80, 0]),
        "empty": b"",
    }


def mscsession_seeds():
    # Selector bytes with bit 6 set make plausible answers; bits 4-5 pick the
    # CSW status (see `usbmsc::fuzz::Script`).
    return {
        "happy": bytes([0x44] * 600),
        "stalls": bytes([0x40, 0x40, 0x10, 0x44, 0x44, 0x44] * 80),
        "failed_csws": bytes([0x44, 0x44, 0x64] * 150),
        "phase_errors": bytes([0x44, 0x44, 0x74] * 150),
        "raw_bytes": bytes(range(256)) * 2,
        "gone_at_once": bytes([0x82]),
        "empty": b"",
    }


# ---- nvme: Identify pages, completions, PRP scripts, hostile controllers ------

def nvme_seeds():
    """`libs/nvme/src/fuzz.rs` input: a mode byte, then that mode's bytes."""
    controller = bytearray(4096)
    struct.pack_into("<H", controller, 0, 0x1B36)
    controller[4:24] = b"lazyos-seed".ljust(20)
    controller[24:64] = b"QEMU NVMe Ctrl".ljust(40)
    controller[64:72] = b"8.2.2".ljust(8)
    controller[77] = 7
    controller[512], controller[513] = 0x66, 0x44
    struct.pack_into("<I", controller, 516, 256)
    controller[525] = 1
    namespace = bytearray(4096)
    struct.pack_into("<QQQ", namespace, 0, 1 << 15, 1 << 15, 1 << 15)
    namespace[25] = 1
    namespace[26] = 1
    struct.pack_into("<II", namespace, 128, 9 << 16, 12 << 16)
    completion = struct.pack("<IIHHHH", 0, 0, 3, 1, 0x0009, 1)
    # A CAP with the NVM command set, TO = 1, MQES = 63, then a ready CSTS.
    cap = struct.pack("<II", 63 | 1 << 24, 1 << 5)
    return {
        "identify_controller": bytes([0]) + bytes(controller),
        "identify_namespace": bytes([1]) + bytes(namespace),
        "completions": bytes([2]) + completion * 8,
        "plan_scattered": bytes([3, 0, 7, 3, 0x10, 0x02, 0, 4, 1, 0x34, 0x12, 1]),
        "plan_aligned": bytes([3, 1, 15, 2, 0, 0, 0, 7, 0, 0, 0, 3, 0]),
        "hostile_ready": bytes([4]) + cap + struct.pack("<I", 1) * 16,
        "hostile_ones": bytes([4]) + b"\xff" * 64,
        "empty": b"",
    }
