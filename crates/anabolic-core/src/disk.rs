// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Physical disks: what they are, how busy, and the partitions and volumes
//! on them.

#[cfg_attr(windows, path = "windows/disk.rs")]
#[cfg_attr(target_os = "linux", path = "linux/disk.rs")]
mod imp;
pub use imp::*;

#[derive(Clone, Debug, Default)]
pub struct Volume {
    /// "C:"
    pub letter: String,
    pub label: String,
    pub total: u64,
    pub free: u64,
}

/// How a disk's partitions are laid out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PartitionStyle {
    /// No partition table: a blank disk, or one file system on all of it.
    #[default]
    None,
    Mbr,
    Gpt,
}

impl PartitionStyle {
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Mbr => "MBR",
            Self::Gpt => "GPT",
        }
    }
}

/// What a partition is for, in broad strokes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PartitionRole {
    /// Files: Windows' basic data, a Linux file system, APFS.
    #[default]
    Data,
    /// What the firmware starts: EFI system, BIOS boot, /boot.
    Boot,
    /// A recovery environment.
    Recovery,
    /// Not for files: Microsoft reserved, metadata, vendor tools.
    Reserved,
    Swap,
    /// LUKS or BitLocker: unreadable until unlocked.
    Encrypted,
    /// A building block of other volumes: LVM, RAID, dynamic disks,
    /// Storage Spaces.
    Pool,
    /// An MBR extended partition, which only holds the logical ones.
    Extended,
}

impl PartitionRole {
    pub fn name(self) -> &'static str {
        match self {
            Self::Data => "Data",
            Self::Boot => "Boot",
            Self::Recovery => "Recovery",
            Self::Reserved => "Reserved",
            Self::Swap => "Swap",
            Self::Encrypted => "Encrypted",
            Self::Pool => "Volume manager",
            Self::Extended => "Extended",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Partition {
    /// As the partition table numbers it, from 1.
    pub number: u32,
    /// The kernel's name for it (nvme0n1p2); empty on Windows.
    pub device: String,
    /// Bytes from the start of the disk.
    pub offset: u64,
    pub size: u64,
    /// What its type says it is: "EFI system", "Linux swap".
    pub kind: String,
    pub role: PartitionRole,
    /// The name GPT keeps for it ("Basic data partition").
    pub name: String,
    /// The file system's label.
    pub label: String,
    /// "NTFS", "ext4"; for encryption or a volume manager, what it holds
    /// when that is known ("ext4 in LUKS").
    pub file_system: String,
    /// Drive letters and folders on Windows, mount points on Linux
    /// ("[SWAP]" for swap in use).
    pub mounts: Vec<String>,
    /// Size and room left of what is mounted from it; 0 when nothing is.
    pub total: u64,
    pub free: u64,
    /// Holds the running system: / or the Windows volume.
    pub system: bool,
}

#[derive(Clone, Debug, Default)]
pub struct DiskStats {
    pub number: u32,
    pub model: String,
    pub bus: &'static str,
    pub ssd: Option<bool>,
    pub size: u64,
    pub volumes: Vec<Volume>,
    pub style: PartitionStyle,
    /// In the order they sit on the disk.
    pub partitions: Vec<Partition>,
    pub read_rate: f64,
    pub write_rate: f64,
    pub read_iops: f64,
    pub write_iops: f64,
    /// Share of the interval the disk had I/O outstanding, 0..=100.
    pub active: f32,
    pub response_ms: f32,
    pub queue: u32,
    pub temperature: Option<i16>,
    /// Where the drive warns that it is hot, and where it is in danger, as
    /// it reports them (Windows; Linux has them from the drive's hardware
    /// monitor).
    pub temperature_warning: Option<i16>,
    pub temperature_critical: Option<i16>,
}

/// GPT partition types, by GUID as `GUID::from_u128` reads it.
const GPT_TYPES: &[(u128, &str, PartitionRole)] = {
    use PartitionRole::*;
    &[
        (0xc12a7328_f81f_11d2_ba4b_00a0c93ec93b, "EFI system", Boot),
        (0x21686148_6449_6e6f_744e_656564454649, "BIOS boot", Boot),
        (
            0xe3c9e316_0b5c_4db8_817d_f92df00215ae,
            "Microsoft reserved",
            Reserved,
        ),
        (
            0xebd0a0a2_b9e5_4433_87c0_68b6b72699c7,
            "Microsoft basic data",
            Data,
        ),
        (
            0xde94bba4_06d1_4d40_a16a_bfd50179d6ac,
            "Windows recovery",
            Recovery,
        ),
        (
            0x5808c8aa_7e8f_42e0_85d2_e1e90434cfb3,
            "Windows LDM metadata",
            Reserved,
        ),
        (
            0xaf9b60a0_1431_4f62_bc68_3311714a69ad,
            "Windows LDM data",
            Pool,
        ),
        (
            0xe75caf8f_f680_4cee_afa3_b001e56efc2d,
            "Storage Spaces",
            Pool,
        ),
        (
            0x0fc63daf_8483_4772_8e79_3d69d8477de4,
            "Linux filesystem",
            Data,
        ),
        (
            0x4f68bce3_e8cd_4db1_96e7_fbcaf984b709,
            "Linux root (x86-64)",
            Data,
        ),
        (
            0x44479540_f297_41b2_9af7_d131d5f0458a,
            "Linux root (x86)",
            Data,
        ),
        (
            0xb921b045_1df0_41c3_af44_4c6f280d3fae,
            "Linux root (ARM64)",
            Data,
        ),
        (
            0x8484680c_9521_48c6_9c11_b0720656f69e,
            "Linux /usr (x86-64)",
            Data,
        ),
        (0x933ac7e1_2eb4_4f13_b844_0e14e2aef915, "Linux home", Data),
        (
            0x3b8f8425_20e0_4f3b_907f_1a25a76f98e8,
            "Linux server data",
            Data,
        ),
        (
            0x4d21b016_b534_45c2_a9fb_5c16e091fd2d,
            "Linux variable data",
            Data,
        ),
        (
            0xbc13c2ff_59e6_4262_a352_b275fd6f7172,
            "Linux extended boot",
            Boot,
        ),
        (0x0657fd6d_a4ab_43c4_84e5_0933c84b4f4f, "Linux swap", Swap),
        (0xe6d6d379_f507_44c2_a23c_238f2a3df928, "Linux LVM", Pool),
        (0xa19d880f_05fc_4d3b_a006_743f0f84911e, "Linux RAID", Pool),
        (
            0xca7d7ccb_63ed_4c53_861c_1742536059cc,
            "Linux LUKS",
            Encrypted,
        ),
        (
            0x7ffec5c9_2d00_49b7_8941_3ea10a5586b7,
            "Linux dm-crypt",
            Encrypted,
        ),
        (0x7c3457ef_0000_11aa_aa11_00306543ecac, "Apple APFS", Data),
        (0x48465300_0000_11aa_aa11_00306543ecac, "Apple HFS+", Data),
        (0x426f6f74_0000_11aa_aa11_00306543ecac, "Apple boot", Boot),
        (0x6a898cc3_1dd2_11b2_99a6_080020736631, "ZFS", Data),
        (0x516e7cb4_6ecf_11d6_8ff8_00022d09712b, "FreeBSD data", Data),
        (0x516e7cb6_6ecf_11d6_8ff8_00022d09712b, "FreeBSD UFS", Data),
        (0x516e7cba_6ecf_11d6_8ff8_00022d09712b, "FreeBSD ZFS", Data),
        (0x516e7cb5_6ecf_11d6_8ff8_00022d09712b, "FreeBSD swap", Swap),
        (0x83bd6b9d_7f41_11dc_be0b_001560b84f0f, "FreeBSD boot", Boot),
    ]
};

/// What a GPT partition's type GUID says it is; unknown ones by their GUID.
pub fn gpt_kind(guid: u128) -> (String, PartitionRole) {
    GPT_TYPES
        .iter()
        .find(|t| t.0 == guid)
        .map_or((guid_text(guid), PartitionRole::Data), |t| {
            (t.1.to_string(), t.2)
        })
}

/// "c12a7328-f81f-11d2-ba4b-00a0c93ec93b", in any case, as a number.
pub fn parse_guid(text: &str) -> Option<u128> {
    let hex: String = text.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 {
        return None;
    }
    u128::from_str_radix(&hex, 16).ok()
}

pub fn guid_text(guid: u128) -> String {
    let hex = format!("{guid:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// What an MBR partition's type byte says it is.
pub fn mbr_kind(byte: u8) -> (String, PartitionRole) {
    use PartitionRole::*;
    let (kind, role) = match byte {
        0x01 => ("FAT12", Data),
        0x04 | 0x06 | 0x0e => ("FAT16", Data),
        0x07 => ("NTFS/exFAT", Data),
        0x0b | 0x0c => ("FAT32", Data),
        0x05 | 0x0f | 0x85 => ("Extended", Extended),
        0x12 => ("Vendor recovery", Recovery),
        0x17 => ("Hidden NTFS", Recovery),
        0x1b | 0x1c => ("Hidden FAT32", Reserved),
        0x27 => ("Windows recovery", Recovery),
        0x42 => ("Windows dynamic", Pool),
        0x82 => ("Linux swap", Swap),
        0x83 => ("Linux", Data),
        0x84 | 0xa0 => ("Hibernation", Reserved),
        0x8e => ("Linux LVM", Pool),
        0xa5 => ("FreeBSD", Data),
        0xa6 => ("OpenBSD", Data),
        0xaf => ("Apple HFS+", Data),
        0xde => ("Dell utility", Reserved),
        0xee => ("GPT protective", Reserved),
        0xef => ("EFI system", Boot),
        0xfb => ("VMware VMFS", Data),
        0xfd => ("Linux RAID", Pool),
        _ => return (format!("Type 0x{byte:02x}"), Data),
    };
    (kind.to_string(), role)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_partition_types() {
        let efi = parse_guid("C12A7328-F81F-11D2-BA4B-00A0C93EC93B").unwrap();
        assert_eq!(gpt_kind(efi), ("EFI system".into(), PartitionRole::Boot));
        let swap = parse_guid("0657fd6d-a4ab-43c4-84e5-0933c84b4f4f").unwrap();
        assert_eq!(gpt_kind(swap).1, PartitionRole::Swap);
        let odd = "01234567-89ab-cdef-0123-456789abcdef";
        assert_eq!(gpt_kind(parse_guid(odd).unwrap()).0, odd);
        assert_eq!(parse_guid("not a guid"), None);
        assert_eq!(mbr_kind(0x83).0, "Linux");
        assert_eq!(mbr_kind(0x0f).1, PartitionRole::Extended);
        assert_eq!(mbr_kind(0x99).0, "Type 0x99");
    }
}
