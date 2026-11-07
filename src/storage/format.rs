//! Explicit whole-device FAT32 quick format. No allocation or implicit formatting.
//! BPB/FAT/FSInfo layout checked against ESP-IDF FatFs src/ff.c f_mkfs.
use super::{BlockDevice, Error};

#[derive(Clone, Copy, Debug)]
pub struct Plan {
    pub start: u32,
    pub sectors: u32,
    pub sectors_per_cluster: u8,
    pub reserved: u16,
    pub fat_sectors: u32,
    pub data_start: u32,
    pub clusters: u32,
}

impl Plan {
    pub fn new(device_sectors: u64) -> Result<Self, &'static str> {
        if !(70_000..=u64::from(u32::MAX)).contains(&device_sectors) {
            return Err("FAT32/MBR device size (512-byte sectors)");
        }
        let start = 2048;
        let sectors = device_sectors as u32 - start;
        let spc = if device_sectors < 1_048_576 {
            1
        } else if device_sectors < 16_777_216 {
            8
        } else {
            64
        };
        // Conservative table size before data alignment; padding can only
        // reduce the number of clusters, so the table remains large enough.
        let fat_sectors = ((u64::from(sectors - 32) / spc + 2).div_ceil(128)) as u32;
        let unaligned = start + 32 + 2 * fat_sectors;
        let reserved = 32 + (2048 - unaligned % 2048) % 2048;
        let data_start = reserved + 2 * fat_sectors;
        let clusters = (sectors - data_start) / spc as u32;
        if !(65_525..=0x0fff_ffee).contains(&clusters) {
            return Err("FAT32 cluster count");
        }
        Ok(Self {
            start,
            sectors,
            sectors_per_cluster: spc as u8,
            reserved: reserved as u16,
            fat_sectors,
            data_start,
            clusters,
        })
    }
}

fn put16(out: &mut [u8], at: usize, value: u16) {
    out[at..at + 2].copy_from_slice(&value.to_le_bytes());
}
fn put32(out: &mut [u8], at: usize, value: u32) {
    out[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// Destroys the partition table and old filesystem. Initializes both FATs,
/// the reserved area and root directory, not the contents of free clusters.
/// Writes the MBR last; failure leaves an incomplete format, never auto-retries.
/// Device writes must complete synchronously (including card busy release).
pub fn quick_format<D: BlockDevice>(
    device: &mut D,
    work: &mut [u8],
    volume_id: u32,
    mut progress: impl FnMut(u32, u32),
) -> Result<Plan, Error<D::Error>> {
    let plan = Plan::new(device.sectors()).map_err(Error::Unsupported)?;
    let chunk = work.len().min(device.max_transfer_bytes()) / 512 * 512;
    if chunk < 512 {
        return Err(Error::Invalid("format scratch/transfer size"));
    }
    let work = &mut work[..chunk];
    work.fill(0);
    device.write(0, &work[..512]).map_err(Error::Device)?;
    // Includes two FATs and the single root cluster. Data beyond it is untouched.
    let total = plan.data_start + u32::from(plan.sectors_per_cluster);
    let mut done = 0;
    while done < total {
        let count = (total - done).min((chunk / 512) as u32);
        device
            .write(u64::from(plan.start + done), &work[..count as usize * 512])
            .map_err(Error::Device)?;
        done += count;
        progress(done, total);
    }
    // FAT[0] media descriptor, FAT[1] clean/no-error flags, FAT[2] root EOC.
    put32(work, 0, 0x0fff_fff8);
    put32(work, 4, 0x0fff_ffff);
    put32(work, 8, 0x0fff_ffff);
    for copy in 0..2 {
        device
            .write(
                u64::from(plan.start + u32::from(plan.reserved) + copy * plan.fat_sectors),
                &work[..512],
            )
            .map_err(Error::Device)?;
    }
    work.fill(0);
    put32(work, 0, 0x4161_5252);
    put32(work, 484, 0x6141_7272);
    put32(work, 488, plan.clusters - 1);
    put32(work, 492, 3);
    put32(work, 508, 0xaa55_0000);
    for offset in [1, 7] {
        device
            .write(u64::from(plan.start + offset), &work[..512])
            .map_err(Error::Device)?;
    }
    work.fill(0);
    work[..3].copy_from_slice(&[0xeb, 0x58, 0x90]);
    work[3..11].copy_from_slice(b"P4RUST  ");
    put16(work, 11, 512);
    work[13] = plan.sectors_per_cluster;
    put16(work, 14, plan.reserved);
    work[16] = 2;
    work[21] = 0xf8;
    put16(work, 24, 63);
    put16(work, 26, 255);
    put32(work, 28, plan.start);
    put32(work, 32, plan.sectors);
    put32(work, 36, plan.fat_sectors);
    put32(work, 44, 2);
    put16(work, 48, 1);
    put16(work, 50, 6);
    work[64] = 0x80;
    work[66] = 0x29;
    put32(work, 67, volume_id);
    work[71..82].copy_from_slice(b"P4SD       ");
    work[82..90].copy_from_slice(b"FAT32   ");
    put16(work, 510, 0xaa55);
    for offset in [6, 0] {
        device
            .write(u64::from(plan.start + offset), &work[..512])
            .map_err(Error::Device)?;
    }
    // Matching volume-label directory entry, with all remaining entries empty.
    work.fill(0);
    work[..11].copy_from_slice(b"P4SD       ");
    work[11] = 8;
    device
        .write(u64::from(plan.start + plan.data_start), &work[..512])
        .map_err(Error::Device)?;
    work.fill(0);
    put32(work, 440, volume_id);
    work[447..450].copy_from_slice(&[0xfe, 0xff, 0xff]);
    work[450] = 0x0c;
    work[451..454].copy_from_slice(&[0xfe, 0xff, 0xff]);
    put32(work, 454, plan.start);
    put32(work, 458, plan.sectors);
    put16(work, 510, 0xaa55);
    device.write(0, &work[..512]).map_err(Error::Device)?;
    Ok(plan)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::storage::{fat32::Fat32, Partition};
    use std::{collections::BTreeMap, vec::Vec};

    struct Disk {
        sectors: u64,
        blocks: BTreeMap<u64, [u8; 512]>,
        writes: Vec<(u64, usize)>,
        fail: Option<usize>,
    }
    impl Disk {
        fn new(sectors: u64) -> Self {
            Self {
                sectors,
                blocks: BTreeMap::new(),
                writes: Vec::new(),
                fail: None,
            }
        }
    }
    impl BlockDevice for Disk {
        type Error = ();
        fn sectors(&self) -> u64 {
            self.sectors
        }
        fn max_transfer_bytes(&self) -> usize {
            8192
        }
        fn read(&mut self, lba: u64, out: &mut [u8]) -> Result<(), ()> {
            assert!(
                out.len().is_multiple_of(512) && lba + (out.len() / 512) as u64 <= self.sectors
            );
            for (i, sector) in out.as_chunks_mut::<512>().0.iter_mut().enumerate() {
                *sector = *self.blocks.get(&(lba + i as u64)).unwrap_or(&[0; 512]);
            }
            Ok(())
        }
        fn write(&mut self, lba: u64, data: &[u8]) -> Result<(), ()> {
            assert!(!data.is_empty() && data.len().is_multiple_of(512) && data.len() <= 8192);
            assert!(lba + (data.len() / 512) as u64 <= self.sectors);
            if self.fail == Some(self.writes.len()) {
                return Err(());
            }
            self.writes.push((lba, data.len()));
            for (i, sector) in data.as_chunks::<512>().0.iter().enumerate() {
                if *sector == [0; 512] {
                    self.blocks.remove(&(lba + i as u64));
                } else {
                    self.blocks.insert(lba + i as u64, *sector);
                }
            }
            Ok(())
        }
    }

    #[test]
    fn geometry_bounds_and_alignment() {
        for sectors in [
            70_000,
            2_097_152,
            16_777_216,
            488_669_184,
            u64::from(u32::MAX),
        ] {
            let p = Plan::new(sectors).unwrap();
            assert_eq!(u64::from(p.start) + u64::from(p.sectors), sectors);
            assert_eq!((p.start + p.data_start) % 2048, 0);
            assert!(p.fat_sectors as u64 * 128 >= p.clusters as u64 + 2);
            assert!(
                p.data_start as u64 + p.clusters as u64 * p.sectors_per_cluster as u64
                    <= p.sectors as u64
            );
        }
        for size in [0, 65_536, u64::from(u32::MAX) + 1, u64::MAX] {
            assert!(Plan::new(size).is_err());
        }
    }

    #[test]
    fn format_large_card_clears_metadata_preserves_free_data_and_remounts() {
        let mut disk = Disk::new(488_669_184);
        let p = Plan::new(disk.sectors).unwrap();
        let root = u64::from(p.start + p.data_start);
        disk.blocks.insert(root, [0xa5; 512]);
        disk.blocks
            .insert(u64::from(p.start) + u64::from(p.reserved) + 17, [0xa5; 512]);
        let untouched = root + u64::from(p.sectors_per_cluster);
        disk.blocks.insert(untouched, [0x73; 512]);
        quick_format(&mut disk, &mut [0; 8192], 0x12345678, |_, _| {}).unwrap();
        assert_eq!(disk.writes.first(), Some(&(0, 512)));
        assert_eq!(disk.writes.last(), Some(&(0, 512)));
        assert_eq!(disk.blocks.get(&untouched), Some(&[0x73; 512]));
        assert!(!disk
            .blocks
            .contains_key(&(u64::from(p.start) + u64::from(p.reserved) + 17)));
        assert_eq!(
            disk.blocks.get(&u64::from(p.start)),
            disk.blocks.get(&u64::from(p.start + 6))
        );
        assert_eq!(
            disk.blocks.get(&u64::from(p.start + 1)),
            disk.blocks.get(&u64::from(p.start + 7))
        );
        let first = u64::from(p.start) + u64::from(p.reserved);
        assert_eq!(
            disk.blocks.get(&first),
            disk.blocks.get(&(first + u64::from(p.fat_sectors)))
        );
        let mut fs = Fat32::mount(Partition::mbr(disk, 0).unwrap()).unwrap();
        assert_eq!(fs.geometry().clusters, p.clusters);
        let mut entries = 0;
        fs.read_dir("/", |_| {
            entries += 1;
            true
        })
        .unwrap();
        assert_eq!(entries, 0);
        fs.create_new("CONFIG.TXT", b"formatted=true\n").unwrap();
        let mut fs = Fat32::mount(fs.into_inner()).unwrap();
        let mut bytes = [0; 15];
        assert_eq!(fs.open("CONFIG.TXT").unwrap().read(&mut bytes).unwrap(), 15);
        assert_eq!(&bytes, b"formatted=true\n");
        // Optional sparse partition image for independent fsck.fat -n validation.
        if let Some(path) = std::env::var_os("P4_FORMAT_IMAGE") {
            use std::io::{Seek, SeekFrom, Write};
            let disk = fs.into_inner().into_inner();
            let mut file = std::fs::File::create(path).unwrap();
            file.set_len(u64::from(p.sectors) * 512).unwrap();
            for (lba, bytes) in disk.blocks {
                if lba >= u64::from(p.start) {
                    file.seek(SeekFrom::Start((lba - u64::from(p.start)) * 512))
                        .unwrap();
                    file.write_all(&bytes).unwrap();
                }
            }
        }
    }

    #[test]
    fn invalid_arguments_do_not_write_and_failed_format_stays_unpublished() {
        let mut disk = Disk::new(488_669_184);
        assert!(quick_format(&mut disk, &mut [0; 511], 1, |_, _| {}).is_err());
        assert!(disk.writes.is_empty());
        disk.fail = Some(3);
        assert!(quick_format(&mut disk, &mut [0; 8192], 1, |_, _| {}).is_err());
        assert!(!disk.blocks.contains_key(&0));
        assert!(Partition::mbr(disk, 0).is_err());
    }
}
