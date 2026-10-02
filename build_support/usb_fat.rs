//! Make the stick's FAT boot partition what strict firmware expects.
//!
//! `bootloader` formats the partition with `fatfs` 0.3, which writes a long
//! name entry before the `.` and `..` entries of every subdirectory, so
//! `efi/` and `efi/boot/` do not start with `.` and `..` as the FAT
//! specification requires (`fsck.fat` reports them), and it labels the volume
//! with the kernel file's lowercase stem. OVMF does not mind, but the stick is
//! for an AMI firmware we cannot test against, so [`tidy`] rewrites the two
//! slots in place: `.` and `..` move to slots 0 and 1 and the stray long-name
//! slots become deleted entries (legal anywhere in a directory). The volume
//! label becomes `LAZYOS` in the boot sector and in the root directory, which
//! is also how the stick shows up on Windows.
//!
//! The partition is the build's own output, but it is still parsed with
//! bounds: every offset is checked against the buffer and the directory walk
//! is depth-limited, so a malformed image is an error, never a panic.

/// The volume label written to the boot sector and the root directory.
pub const LABEL: &[u8; 11] = b"LAZYOS     ";

const ENTRY: usize = 32;
const ATTR_LFN: u8 = 0x0F;
const ATTR_DIR: u8 = 0x10;
const ATTR_LABEL: u8 = 0x08;
const DELETED: u8 = 0xE5;
/// `efi/boot` is two levels deep; anything deeper is not ours.
const MAX_DEPTH: usize = 4;
/// How many directories the walk visits at most (the partition has two).
const MAX_DIRS: usize = 64;

/// The geometry [`tidy`] needs from the boot sector.
struct Bpb {
    cluster_bytes: usize,
    /// Byte offsets of the root directory and of cluster 2.
    root: usize,
    root_bytes: usize,
    data: usize,
    fat32: bool,
}

fn le16(bytes: &[u8], at: usize) -> Result<usize, String> {
    bytes
        .get(at..at + 2)
        .map(|b| usize::from(u16::from_le_bytes([b[0], b[1]])))
        .ok_or_else(|| "the FAT image is truncated".to_string())
}

fn le32(bytes: &[u8], at: usize) -> Result<usize, String> {
    bytes
        .get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
        .ok_or_else(|| "the FAT image is truncated".to_string())
}

fn parse(volume: &[u8]) -> Result<Bpb, String> {
    let sector = le16(volume, 11)?;
    let per_cluster = usize::from(*volume.get(13).ok_or("truncated")?);
    let reserved = le16(volume, 14)?;
    let fats = usize::from(*volume.get(16).ok_or("truncated")?);
    let root_entries = le16(volume, 17)?;
    let fat16_size = le16(volume, 22)?;
    let fat32 = fat16_size == 0;
    let fat_size = if fat32 { le32(volume, 36)? } else { fat16_size };
    if !matches!(sector, 512 | 1024 | 2048 | 4096) || per_cluster == 0 || fats == 0 {
        return Err("not a FAT boot sector".into());
    }
    let root = (reserved + fats * fat_size) * sector;
    let root_bytes = root_entries * ENTRY;
    Ok(Bpb {
        cluster_bytes: sector * per_cluster,
        root,
        root_bytes,
        data: root + root_bytes,
        fat32,
    })
}

/// Fix the `.`/`..` slots of every subdirectory reachable from the root
/// (first cluster of each) and relabel the volume. Returns how many
/// directories were rewritten.
pub fn tidy(volume: &mut [u8]) -> Result<usize, String> {
    let bpb = parse(volume)?;
    let label_at = if bpb.fat32 { 71 } else { 43 };
    volume
        .get_mut(label_at..label_at + 11)
        .ok_or("truncated")?
        .copy_from_slice(LABEL);
    let root = if bpb.fat32 {
        cluster_range(&bpb, le32(volume, 44)?, volume.len())?
    } else {
        bpb.root..bpb.root + bpb.root_bytes
    };
    if root.end > volume.len() {
        return Err("the root directory runs past the image".into());
    }
    relabel_root(&mut volume[root.clone()]);
    walk(volume, &bpb, root, 0, &mut { MAX_DIRS })
}

fn cluster_range(bpb: &Bpb, cluster: usize, len: usize) -> Result<std::ops::Range<usize>, String> {
    let start = cluster
        .checked_sub(2)
        .and_then(|index| index.checked_mul(bpb.cluster_bytes))
        .and_then(|offset| offset.checked_add(bpb.data))
        .ok_or("a directory names a reserved cluster")?;
    let end = start + bpb.cluster_bytes;
    if end > len {
        return Err("a directory cluster runs past the image".into());
    }
    Ok(start..end)
}

fn relabel_root(root: &mut [u8]) {
    for entry in root.chunks_exact_mut(ENTRY) {
        if entry[0] == 0 {
            break;
        }
        if entry[0] != DELETED && entry[11] & ATTR_LFN == ATTR_LABEL {
            entry[..11].copy_from_slice(LABEL);
            return;
        }
    }
}

/// Fix the subdirectories listed in `dir` (a byte range of `volume`), then
/// recurse into them.
fn walk(
    volume: &mut [u8],
    bpb: &Bpb,
    dir: std::ops::Range<usize>,
    depth: usize,
    budget: &mut usize,
) -> Result<usize, String> {
    if depth > MAX_DEPTH {
        return Err("the boot partition nests directories too deeply".into());
    }
    *budget = budget
        .checked_sub(1)
        .ok_or("the boot partition has too many directories")?;
    let mut children = Vec::new();
    for at in dir.step_by(ENTRY) {
        let entry = &volume[at..at + ENTRY];
        if entry[0] == 0 {
            break;
        }
        let name = &entry[..11];
        let is_dot = name == b".          " || name == b"..         ";
        if entry[0] != DELETED && entry[11] != ATTR_LFN && entry[11] & ATTR_DIR != 0 && !is_dot {
            let high = if bpb.fat32 { le16(entry, 20)? << 16 } else { 0 };
            children.push(high | le16(entry, 26)?);
        }
    }
    let mut fixed = 0;
    for cluster in children {
        let range = cluster_range(bpb, cluster, volume.len())?;
        fixed += usize::from(fix_dots(&mut volume[range.clone()]));
        fixed += walk(volume, bpb, range, depth + 1, budget)?;
    }
    Ok(fixed)
}

/// Move `.` and `..` to slots 0 and 1 when long-name entries precede them.
fn fix_dots(cluster: &mut [u8]) -> bool {
    let slots: Vec<&[u8]> = cluster.chunks_exact(ENTRY).take(4).collect();
    let is = |slot: &[u8], name: &[u8; 11]| slot[..11] == *name && slot[11] & ATTR_DIR != 0;
    let lfn = |slot: &[u8]| slot[11] == ATTR_LFN && slot[0] != DELETED;
    if slots.len() < 4
        || !(lfn(slots[0])
            && is(slots[1], b".          ")
            && lfn(slots[2])
            && is(slots[3], b"..         "))
    {
        return false;
    }
    let dot: Vec<u8> = slots[1].to_vec();
    let dotdot: Vec<u8> = slots[3].to_vec();
    cluster[..ENTRY].copy_from_slice(&dot);
    cluster[ENTRY..2 * ENTRY].copy_from_slice(&dotdot);
    for slot in 2..4 {
        let entry = &mut cluster[slot * ENTRY..(slot + 1) * ENTRY];
        entry.fill(0);
        entry[0] = DELETED;
    }
    true
}
