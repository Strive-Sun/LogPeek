use anyhow::{anyhow, bail, Context};
use std::collections::{HashMap, HashSet};

#[cfg(windows)]
pub mod ipc;
#[cfg(windows)]
mod volume;
#[cfg(windows)]
pub use volume::{
    enumerate_mft, query_usn_journal, read_usn_journal, MftEnumeration, UsnJournalInfo,
    UsnReadSummary,
};

pub const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const ENUM_HEADER_SIZE: usize = 8;
const USN_V2_MIN_SIZE: usize = 60;
const USN_V3_MIN_SIZE: usize = 76;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileId([u8; 16]);

impl FileId {
    pub fn from_u64(value: u64) -> Self {
        let mut bytes = [0_u8; 16];
        bytes[..8].copy_from_slice(&value.to_le_bytes());
        Self(bytes)
    }

    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(self) -> [u8; 16] {
        self.0
    }

    fn is_ntfs_root_reference(self) -> bool {
        let record = u64::from_le_bytes(self.0[..8].try_into().unwrap());
        record & 0x0000_ffff_ffff_ffff == 5
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MftRecord {
    pub id: FileId,
    pub parent_id: FileId,
    pub name: String,
    pub attributes: u32,
    pub reason: u32,
    pub usn: i64,
}

impl MftRecord {
    pub fn is_directory(&self) -> bool {
        self.attributes & FILE_ATTRIBUTE_DIRECTORY != 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMftEntry {
    pub id: FileId,
    pub parent_id: FileId,
    pub path: String,
    pub name: String,
    pub attributes: u32,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ResolveDiagnostics {
    pub orphan_records: u64,
    pub cyclic_records: u64,
    pub duplicate_records: u64,
    pub reparse_records: u64,
}

const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// Rebuilds file paths while retaining only the MFT node table and resolved
/// directory paths. Full file paths are delivered in bounded batches so a
/// million-entry volume does not require a second in-memory path snapshot.
pub fn resolve_mft_files_in_batches<F>(
    volume_root: &str,
    records: Vec<MftRecord>,
    batch_size: usize,
    mut on_batch: F,
) -> anyhow::Result<ResolveDiagnostics>
where
    F: FnMut(Vec<ResolvedMftEntry>) -> anyhow::Result<()>,
{
    resolve_mft_files_internal(volume_root, records, batch_size, &mut on_batch)
        .map(|(diagnostics, _)| diagnostics)
}

pub fn resolve_mft_files_in_batches_retain<F>(
    volume_root: &str,
    records: Vec<MftRecord>,
    batch_size: usize,
    mut on_batch: F,
) -> anyhow::Result<(ResolveDiagnostics, Vec<MftRecord>)>
where
    F: FnMut(Vec<ResolvedMftEntry>) -> anyhow::Result<()>,
{
    resolve_mft_files_internal(volume_root, records, batch_size, &mut on_batch)
}

fn resolve_mft_files_internal<F>(
    volume_root: &str,
    records: Vec<MftRecord>,
    batch_size: usize,
    on_batch: &mut F,
) -> anyhow::Result<(ResolveDiagnostics, Vec<MftRecord>)>
where
    F: FnMut(Vec<ResolvedMftEntry>) -> anyhow::Result<()>,
{
    if batch_size == 0 {
        bail!("MFT 路径批次大小必须大于 0");
    }
    let root = volume_root.trim_end_matches(['\\', '/']).to_string();
    let mut by_id = HashMap::with_capacity(records.len());
    let mut diagnostics = ResolveDiagnostics::default();
    for record in records {
        if by_id.insert(record.id, record).is_some() {
            diagnostics.duplicate_records += 1;
        }
    }
    let file_ids = by_id
        .values()
        .filter(|record| !record.is_directory())
        .map(|record| record.id)
        .collect::<Vec<_>>();
    let mut directory_memo = HashMap::<FileId, Option<String>>::new();
    let mut batch = Vec::with_capacity(batch_size);
    for id in file_ids {
        let record = &by_id[&id];
        let mut visiting = HashSet::new();
        let Some(parent) = resolve_directory(
            record.parent_id,
            &root,
            &by_id,
            &mut directory_memo,
            &mut visiting,
            &mut diagnostics,
        ) else {
            continue;
        };
        batch.push(ResolvedMftEntry {
            id,
            parent_id: record.parent_id,
            path: format!("{parent}\\{}", record.name),
            name: record.name.clone(),
            attributes: record.attributes,
        });
        if batch.len() >= batch_size {
            on_batch(std::mem::take(&mut batch))?;
            batch.reserve(batch_size);
        }
    }
    if !batch.is_empty() {
        on_batch(batch)?;
    }
    Ok((diagnostics, by_id.into_values().collect()))
}

fn resolve_directory(
    id: FileId,
    root: &str,
    records: &HashMap<FileId, MftRecord>,
    memo: &mut HashMap<FileId, Option<String>>,
    visiting: &mut HashSet<FileId>,
    diagnostics: &mut ResolveDiagnostics,
) -> Option<String> {
    if let Some(path) = memo.get(&id) {
        return path.clone();
    }
    // FSCTL_ENUM_USN_DATA does not consistently return an explicit record for
    // the NTFS root directory. Its well-known file-record number is 5; the
    // upper 16 bits of a V2 reference are the sequence number.
    if id.is_ntfs_root_reference() {
        return Some(root.to_string());
    }
    let Some(record) = records.get(&id) else {
        diagnostics.orphan_records += 1;
        return None;
    };
    if !record.is_directory() {
        diagnostics.orphan_records += 1;
        return None;
    }
    if record.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        diagnostics.reparse_records += 1;
        memo.insert(id, None);
        return None;
    }
    if !visiting.insert(id) {
        diagnostics.cyclic_records += 1;
        memo.insert(id, None);
        return None;
    }
    let result = if record.id == record.parent_id || record.name == "." {
        Some(root.to_string())
    } else {
        resolve_directory(record.parent_id, root, records, memo, visiting, diagnostics)
            .map(|parent| format!("{parent}\\{}", record.name))
    };
    visiting.remove(&id);
    memo.insert(id, result.clone());
    result
}

pub fn parse_mft_enum_buffer(buffer: &[u8]) -> anyhow::Result<(u64, Vec<MftRecord>)> {
    if buffer.len() < ENUM_HEADER_SIZE {
        bail!("MFT 枚举缓冲区缺少 continuation header");
    }
    let next_reference = read_u64(buffer, 0)?;
    let mut records = Vec::new();
    let mut offset = ENUM_HEADER_SIZE;
    while offset < buffer.len() {
        let record_length = read_u32(buffer, offset)? as usize;
        if record_length == 0 {
            bail!("MFT 记录长度为 0");
        }
        let end = offset
            .checked_add(record_length)
            .filter(|end| *end <= buffer.len())
            .ok_or_else(|| anyhow!("MFT 记录越过返回缓冲区"))?;
        let record = &buffer[offset..end];
        records.push(
            parse_usn_record(record).with_context(|| {
                format!("无法解析偏移 {offset}、长度 {record_length} 的 MFT 记录")
            })?,
        );
        offset = end;
    }
    Ok((next_reference, records))
}

pub fn parse_usn_record(record: &[u8]) -> anyhow::Result<MftRecord> {
    if record.len() < 8 {
        bail!("USN 记录头不完整");
    }
    let declared_length = read_u32(record, 0)? as usize;
    if declared_length != record.len() {
        bail!("USN 记录声明长度与缓冲区不一致");
    }
    let major = read_u16(record, 4)?;
    let (id, parent_id, usn, reason, attributes, name_length, name_offset, minimum) = match major {
        2 => (
            FileId::from_u64(read_u64(record, 8)?),
            FileId::from_u64(read_u64(record, 16)?),
            read_i64(record, 24)?,
            read_u32(record, 40)?,
            read_u32(record, 52)?,
            read_u16(record, 56)? as usize,
            read_u16(record, 58)? as usize,
            USN_V2_MIN_SIZE,
        ),
        3 => (
            FileId::from_bytes(read_array(record, 8)?),
            FileId::from_bytes(read_array(record, 24)?),
            read_i64(record, 40)?,
            read_u32(record, 56)?,
            read_u32(record, 68)?,
            read_u16(record, 72)? as usize,
            read_u16(record, 74)? as usize,
            USN_V3_MIN_SIZE,
        ),
        _ => bail!("不支持的 USN 记录版本 {major}"),
    };
    if record.len() < minimum || name_length % 2 != 0 || name_offset < minimum {
        bail!("USN 记录字段边界无效");
    }
    let name_end = name_offset
        .checked_add(name_length)
        .filter(|end| *end <= record.len())
        .ok_or_else(|| anyhow!("USN 文件名越过记录边界"))?;
    let words = record[name_offset..name_end]
        .chunks_exact(2)
        .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
        .collect::<Vec<_>>();
    // NTFS stores arbitrary UTF-16 code units and existing volumes can contain
    // unpaired surrogates. Preserve enumeration progress with Windows-style
    // lossy display text instead of aborting the entire volume.
    let name = String::from_utf16_lossy(&words);
    if name.contains(['\\', '/']) || name.contains('\0') {
        bail!("USN 文件名包含路径分隔符或 NUL");
    }
    Ok(MftRecord {
        id,
        parent_id,
        name,
        attributes,
        reason,
        usn,
    })
}

pub fn resolve_mft_paths(
    volume_root: &str,
    records: Vec<MftRecord>,
) -> (Vec<ResolvedMftEntry>, ResolveDiagnostics) {
    let root = volume_root.trim_end_matches(['\\', '/']).to_string();
    let mut by_id = HashMap::with_capacity(records.len());
    let mut diagnostics = ResolveDiagnostics::default();
    for record in records {
        if by_id.insert(record.id, record).is_some() {
            diagnostics.duplicate_records += 1;
        }
    }
    let mut memo = HashMap::<FileId, Option<String>>::with_capacity(by_id.len());
    let ids = by_id.keys().copied().collect::<Vec<_>>();
    let mut entries = Vec::with_capacity(ids.len());
    for id in ids {
        let mut visiting = HashSet::new();
        let Some(path) = resolve_one(
            id,
            &root,
            &by_id,
            &mut memo,
            &mut visiting,
            &mut diagnostics,
        ) else {
            continue;
        };
        let record = &by_id[&id];
        if record.id == record.parent_id || record.name == "." {
            continue;
        }
        entries.push(ResolvedMftEntry {
            id,
            parent_id: record.parent_id,
            path,
            name: record.name.clone(),
            attributes: record.attributes,
        });
    }
    entries.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    (entries, diagnostics)
}

fn resolve_one(
    id: FileId,
    root: &str,
    records: &HashMap<FileId, MftRecord>,
    memo: &mut HashMap<FileId, Option<String>>,
    visiting: &mut HashSet<FileId>,
    diagnostics: &mut ResolveDiagnostics,
) -> Option<String> {
    if let Some(path) = memo.get(&id) {
        return path.clone();
    }
    let Some(record) = records.get(&id) else {
        diagnostics.orphan_records += 1;
        return None;
    };
    if !visiting.insert(id) {
        diagnostics.cyclic_records += 1;
        memo.insert(id, None);
        return None;
    }
    let result = if record.id == record.parent_id || record.name == "." {
        Some(root.to_string())
    } else {
        resolve_one(record.parent_id, root, records, memo, visiting, diagnostics)
            .map(|parent| format!("{parent}\\{}", record.name))
    };
    visiting.remove(&id);
    memo.insert(id, result.clone());
    result
}

fn read_u16(bytes: &[u8], offset: usize) -> anyhow::Result<u16> {
    Ok(u16::from_le_bytes(read_array(bytes, offset)?))
}

fn read_u32(bytes: &[u8], offset: usize) -> anyhow::Result<u32> {
    Ok(u32::from_le_bytes(read_array(bytes, offset)?))
}

fn read_u64(bytes: &[u8], offset: usize) -> anyhow::Result<u64> {
    Ok(u64::from_le_bytes(read_array(bytes, offset)?))
}

fn read_i64(bytes: &[u8], offset: usize) -> anyhow::Result<i64> {
    Ok(i64::from_le_bytes(read_array(bytes, offset)?))
}

fn read_array<const N: usize>(bytes: &[u8], offset: usize) -> anyhow::Result<[u8; N]> {
    bytes
        .get(offset..offset + N)
        .ok_or_else(|| anyhow!("字段越过记录边界"))?
        .try_into()
        .map_err(|_| anyhow!("字段长度无效"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v2_record(id: u64, parent: u64, name: &str, attributes: u32) -> Vec<u8> {
        let name = name.encode_utf16().collect::<Vec<_>>();
        let length = USN_V2_MIN_SIZE + name.len() * 2;
        let mut record = vec![0_u8; length];
        record[0..4].copy_from_slice(&(length as u32).to_le_bytes());
        record[4..6].copy_from_slice(&2_u16.to_le_bytes());
        record[8..16].copy_from_slice(&id.to_le_bytes());
        record[16..24].copy_from_slice(&parent.to_le_bytes());
        record[24..32].copy_from_slice(&(id as i64).to_le_bytes());
        record[52..56].copy_from_slice(&attributes.to_le_bytes());
        record[56..58].copy_from_slice(&((name.len() * 2) as u16).to_le_bytes());
        record[58..60].copy_from_slice(&(USN_V2_MIN_SIZE as u16).to_le_bytes());
        for (index, word) in name.into_iter().enumerate() {
            let offset = USN_V2_MIN_SIZE + index * 2;
            record[offset..offset + 2].copy_from_slice(&word.to_le_bytes());
        }
        record
    }

    fn v3_record(id: [u8; 16], parent: [u8; 16], name: &str, reason: u32) -> Vec<u8> {
        let name = name.encode_utf16().collect::<Vec<_>>();
        let length = USN_V3_MIN_SIZE + name.len() * 2;
        let mut record = vec![0_u8; length];
        record[0..4].copy_from_slice(&(length as u32).to_le_bytes());
        record[4..6].copy_from_slice(&3_u16.to_le_bytes());
        record[8..24].copy_from_slice(&id);
        record[24..40].copy_from_slice(&parent);
        record[40..48].copy_from_slice(&123_i64.to_le_bytes());
        record[56..60].copy_from_slice(&reason.to_le_bytes());
        record[68..72].copy_from_slice(&32_u32.to_le_bytes());
        record[72..74].copy_from_slice(&((name.len() * 2) as u16).to_le_bytes());
        record[74..76].copy_from_slice(&(USN_V3_MIN_SIZE as u16).to_le_bytes());
        for (index, word) in name.into_iter().enumerate() {
            let offset = USN_V3_MIN_SIZE + index * 2;
            record[offset..offset + 2].copy_from_slice(&word.to_le_bytes());
        }
        record
    }

    #[test]
    fn parses_v2_enum_batches_and_unicode_names() {
        let record = v2_record(42, 5, "调试.log", 0);
        let mut buffer = 100_u64.to_le_bytes().to_vec();
        buffer.extend(record);
        let (next, records) = parse_mft_enum_buffer(&buffer).unwrap();
        assert_eq!(next, 100);
        assert_eq!(records[0].id, FileId::from_u64(42));
        assert_eq!(records[0].parent_id, FileId::from_u64(5));
        assert_eq!(records[0].name, "调试.log");
    }

    #[test]
    fn parses_fixed_v3_fixture_with_128_bit_references_and_rename_reason() {
        let id = [0x11; 16];
        let parent = [0x22; 16];
        let fixture = v3_record(id, parent, "renamed.log", 0x0000_2000);
        let record = parse_usn_record(&fixture).unwrap();
        assert_eq!(record.id, FileId::from_bytes(id));
        assert_eq!(record.parent_id, FileId::from_bytes(parent));
        assert_eq!(record.name, "renamed.log");
        assert_eq!(record.reason, 0x0000_2000);
        assert_eq!(record.usn, 123);
    }

    #[test]
    fn parser_boundary_fuzz_rejects_malformed_v2_v3_lengths_without_panicking() {
        let fixtures = [
            v2_record(7, 5, "boundary.log", 0),
            v3_record([7; 16], [5; 16], "boundary.log", 0),
        ];
        for fixture in fixtures {
            for cut in 0..fixture.len() {
                let result = std::panic::catch_unwind(|| parse_usn_record(&fixture[..cut]));
                assert!(result.is_ok());
                assert!(result.unwrap().is_err());
            }
            let mut oversized = fixture.clone();
            oversized[0..4].copy_from_slice(&u32::MAX.to_le_bytes());
            assert!(parse_usn_record(&oversized).is_err());
            let mut odd_name = fixture;
            let name_length_offset = if odd_name[4] == 2 { 56 } else { 72 };
            odd_name[name_length_offset..name_length_offset + 2]
                .copy_from_slice(&3_u16.to_le_bytes());
            assert!(parse_usn_record(&odd_name).is_err());
        }
    }

    #[test]
    fn rejects_truncated_unknown_and_path_like_records() {
        assert!(parse_usn_record(&[0; 4]).is_err());
        let mut unknown = v2_record(1, 1, "ok", 0);
        unknown[4..6].copy_from_slice(&9_u16.to_le_bytes());
        assert!(parse_usn_record(&unknown).is_err());
        assert!(parse_usn_record(&v2_record(1, 1, "bad\\name", 0)).is_err());
        let mut truncated = v2_record(1, 1, "ok", 0);
        truncated.pop();
        assert!(parse_usn_record(&truncated).is_err());
    }

    #[test]
    fn resolves_out_of_order_paths_and_reports_bad_graphs() {
        let records = vec![
            MftRecord {
                id: FileId::from_u64(30),
                parent_id: FileId::from_u64(20),
                name: "debug.log".into(),
                attributes: 0,
                reason: 0,
                usn: 3,
            },
            MftRecord {
                id: FileId::from_u64(5),
                parent_id: FileId::from_u64(5),
                name: ".".into(),
                attributes: FILE_ATTRIBUTE_DIRECTORY,
                reason: 0,
                usn: 1,
            },
            MftRecord {
                id: FileId::from_u64(20),
                parent_id: FileId::from_u64(5),
                name: "Logs".into(),
                attributes: FILE_ATTRIBUTE_DIRECTORY,
                reason: 0,
                usn: 2,
            },
            MftRecord {
                id: FileId::from_u64(40),
                parent_id: FileId::from_u64(99),
                name: "orphan.log".into(),
                attributes: 0,
                reason: 0,
                usn: 4,
            },
        ];
        let (entries, diagnostics) = resolve_mft_paths("C:\\", records);
        assert!(entries
            .iter()
            .any(|entry| entry.path == "C:\\Logs\\debug.log"));
        assert!(diagnostics.orphan_records > 0);
    }

    #[test]
    fn streams_only_files_and_skips_reparse_descendants() {
        let records = vec![
            MftRecord {
                id: FileId::from_u64(5),
                parent_id: FileId::from_u64(5),
                name: ".".into(),
                attributes: FILE_ATTRIBUTE_DIRECTORY,
                reason: 0,
                usn: 1,
            },
            MftRecord {
                id: FileId::from_u64(10),
                parent_id: FileId::from_u64(5),
                name: "Logs".into(),
                attributes: FILE_ATTRIBUTE_DIRECTORY,
                reason: 0,
                usn: 2,
            },
            MftRecord {
                id: FileId::from_u64(11),
                parent_id: FileId::from_u64(10),
                name: "app.log".into(),
                attributes: 0,
                reason: 0,
                usn: 3,
            },
            MftRecord {
                id: FileId::from_u64(20),
                parent_id: FileId::from_u64(5),
                name: "Junction".into(),
                attributes: FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT,
                reason: 0,
                usn: 4,
            },
            MftRecord {
                id: FileId::from_u64(21),
                parent_id: FileId::from_u64(20),
                name: "duplicate.log".into(),
                attributes: 0,
                reason: 0,
                usn: 5,
            },
        ];
        let mut entries = Vec::new();
        let diagnostics = resolve_mft_files_in_batches("C:\\", records, 1, |batch| {
            assert_eq!(batch.len(), 1);
            entries.extend(batch);
            Ok(())
        })
        .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "C:\\Logs\\app.log");
        assert_eq!(diagnostics.reparse_records, 1);
    }
}
