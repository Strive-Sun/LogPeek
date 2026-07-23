// Search index architecture and tokenization are adapted from naaive/orange:
// https://github.com/naaive/orange/tree/09cfcdeba08ce718a978c6dadbf9b5d8f41b658b
// Orange is licensed under GNU GPL v3.0. This derivative module is subject
// to GPL v3.0; LogCrate distributions containing it must comply with GPL v3.0.

use anyhow::Context;
use convert_case::{Case, Casing};
use jieba_rs::Jieba;
use pinyin::ToPinyin;
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};
use tantivy::collector::TopDocs;
use tantivy::directory::MmapDirectory;
use tantivy::query::{BooleanQuery, Occur, Query, QueryParser, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, Value, INDEXED, STORED,
};
use tantivy::{doc, Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term};
use zhconv::{zhconv, Variant};

const BULK_COMMIT_INTERVAL: Duration = Duration::from_secs(5);
const INCREMENTAL_COMMIT_INTERVAL: Duration = Duration::from_secs(2);
const COMMIT_DOCUMENTS: usize = 100_000;
const MAX_QUERY_CANDIDATES: usize = 10_000;
const TRUE_BYTES: &[u8] = b"1";
const FALSE_BYTES: &[u8] = b"0";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchIndexEntry {
    pub path: String,
    pub name: String,
    pub is_log: bool,
    pub is_archive: bool,
}

pub struct SearchIndex {
    reader: IndexReader,
    writer: IndexWriter,
    name_field: Field,
    original_name_field: Field,
    path_key_field: Field,
    path_field: Field,
    is_log_field: Field,
    is_archive_field: Field,
    ext_field: Field,
    query_parser: QueryParser,
    tokenizer: Jieba,
    pending_documents: usize,
    last_commit: Instant,
    bulk_indexing: bool,
}

impl SearchIndex {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        fs::create_dir_all(path)?;
        let schema = build_schema();
        let index = Index::open_or_create(MmapDirectory::open(path)?, schema)?;
        let schema = index.schema();
        let field = |name| {
            schema
                .get_field(name)
                .with_context(|| format!("Orange search index is missing field {name}"))
        };
        let name_field = field("name")?;
        let original_name_field = field("original_name")?;
        let path_key_field = field("path_key")?;
        let path_field = field("path")?;
        let is_log_field = field("is_log")?;
        let is_archive_field = field("is_archive")?;
        let ext_field = field("ext")?;
        let writer = index.writer_with_num_threads(2, 140_000_000)?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()?;
        let mut query_parser = QueryParser::for_index(&index, vec![name_field]);
        query_parser.set_field_boost(name_field, 4.0);
        query_parser.set_conjunction_by_default();
        Ok(Self {
            reader,
            writer,
            name_field,
            original_name_field,
            path_key_field,
            path_field,
            is_log_field,
            is_archive_field,
            ext_field,
            query_parser,
            tokenizer: Jieba::new(),
            pending_documents: 0,
            last_commit: Instant::now(),
            bulk_indexing: false,
        })
    }

    pub fn clear(&mut self) -> anyhow::Result<()> {
        self.writer.delete_all_documents()?;
        self.pending_documents = 0;
        self.commit()
    }

    pub fn begin_bulk(&mut self) -> anyhow::Result<()> {
        self.bulk_indexing = true;
        self.clear()
    }

    pub fn finish_bulk(&mut self) -> anyhow::Result<()> {
        self.commit()?;
        self.bulk_indexing = false;
        Ok(())
    }

    pub fn add_batch(&mut self, entries: &[SearchIndexEntry]) -> anyhow::Result<()> {
        for entry in entries {
            self.add_entry(entry)?;
        }
        self.after_documents(entries.len())
    }

    pub fn upsert_batch(&mut self, entries: &[SearchIndexEntry]) -> anyhow::Result<()> {
        for entry in entries {
            self.delete_path(&entry.path);
            self.add_entry(entry)?;
        }
        self.after_documents(entries.len())
    }

    pub fn apply_changes(
        &mut self,
        deleted_paths: &[String],
        upserts: &[SearchIndexEntry],
    ) -> anyhow::Result<()> {
        for path in deleted_paths {
            self.delete_path(path);
        }
        for entry in upserts {
            self.delete_path(&entry.path);
            self.add_entry(entry)?;
        }
        self.pending_documents = self
            .pending_documents
            .saturating_add(deleted_paths.len())
            .saturating_add(upserts.len());
        self.commit()
    }

    #[cfg(test)]
    pub fn delete_paths(&mut self, paths: &[String]) -> anyhow::Result<()> {
        for path in paths {
            self.delete_path(path);
        }
        self.pending_documents = self.pending_documents.saturating_add(paths.len());
        self.commit()
    }

    fn add_entry(&mut self, entry: &SearchIndexEntry) -> anyhow::Result<()> {
        let tokenized_name = self.tokenize(&entry.name);
        let extension = Path::new(&entry.name)
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_lowercase();
        self.writer.add_document(doc!(
            self.name_field => tokenized_name,
            self.original_name_field => entry.name.clone(),
            self.path_key_field => path_key(&entry.path).into_bytes(),
            self.path_field => entry.path.as_bytes().to_vec(),
            self.is_log_field => bool_bytes(entry.is_log).to_vec(),
            self.is_archive_field => bool_bytes(entry.is_archive).to_vec(),
            self.ext_field => extension,
        ))?;
        Ok(())
    }

    fn delete_path(&mut self, path: &str) {
        self.writer.delete_term(Term::from_field_bytes(
            self.path_key_field,
            path_key(path).as_bytes(),
        ));
    }

    fn after_documents(&mut self, documents: usize) -> anyhow::Result<()> {
        self.pending_documents = self.pending_documents.saturating_add(documents);
        let interval = if self.bulk_indexing {
            BULK_COMMIT_INTERVAL
        } else {
            INCREMENTAL_COMMIT_INTERVAL
        };
        if self.pending_documents >= COMMIT_DOCUMENTS || self.last_commit.elapsed() >= interval {
            self.commit()?;
        }
        Ok(())
    }

    pub fn commit(&mut self) -> anyhow::Result<()> {
        self.writer.commit()?;
        self.reader.reload()?;
        self.pending_documents = 0;
        self.last_commit = Instant::now();
        Ok(())
    }

    pub fn search(
        &self,
        terms: &[String],
        filter: &str,
        offset: u32,
        limit: u32,
    ) -> anyhow::Result<(Vec<SearchIndexEntry>, u64)> {
        let keyword = terms.join(" ");
        let tokens = self.search_tokenize(&keyword);
        let keyword_query = self
            .query_parser
            .parse_query(&tokens)
            .with_context(|| format!("Orange query parser rejected {keyword:?}"))?;
        let mut subqueries = vec![(Occur::Must, keyword_query)];
        if filter == "log" {
            subqueries.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_bytes(self.is_log_field, TRUE_BYTES),
                    IndexRecordOption::Basic,
                )) as Box<dyn Query>,
            ));
            subqueries.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_bytes(self.is_archive_field, FALSE_BYTES),
                    IndexRecordOption::Basic,
                )),
            ));
        } else if filter == "archive" {
            subqueries.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_bytes(self.is_archive_field, TRUE_BYTES),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        let query = BooleanQuery::new(subqueries);
        let requested = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .saturating_add(limit as usize)
            .clamp(limit as usize + 1, MAX_QUERY_CANDIDATES);
        let searcher = self.reader.searcher();
        let top_docs = searcher.search(&query, &TopDocs::with_limit(requested))?;
        let mut entries = Vec::with_capacity(top_docs.len());
        for (_, address) in top_docs {
            let document: TantivyDocument = searcher.doc(address)?;
            let Some(path) = document
                .get_first(self.path_field)
                .and_then(|value| value.as_bytes())
                .and_then(|value| std::str::from_utf8(value).ok())
            else {
                continue;
            };
            let Some(name) = document
                .get_first(self.original_name_field)
                .and_then(|value| value.as_str())
            else {
                continue;
            };
            let is_log = document
                .get_first(self.is_log_field)
                .and_then(|value| value.as_bytes())
                == Some(TRUE_BYTES);
            let is_archive = document
                .get_first(self.is_archive_field)
                .and_then(|value| value.as_bytes())
                == Some(TRUE_BYTES);
            entries.push(SearchIndexEntry {
                path: path.to_owned(),
                name: name.to_owned(),
                is_log,
                is_archive,
            });
        }
        let keyword_lower = keyword.to_lowercase();
        entries.sort_by_cached_key(|entry| {
            let name = entry.name.to_lowercase();
            let rank = if name == keyword_lower {
                0
            } else if name.starts_with(&keyword_lower) {
                1
            } else if name.contains(&keyword_lower) {
                2
            } else {
                3
            };
            (rank, name, entry.path.to_lowercase())
        });
        let start = offset as usize;
        let end = start.saturating_add(limit as usize).min(entries.len());
        let has_more = entries.len() > end || requested == MAX_QUERY_CANDIDATES;
        let items = if start < entries.len() {
            entries[start..end].to_vec()
        } else {
            Vec::new()
        };
        Ok((items, (end + usize::from(has_more)) as u64))
    }

    pub fn num_docs(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    fn search_tokenize(&self, value: &str) -> String {
        let value = value.replace(['-', '+', ',', '.', ':', '/', '\\', '_'], " ");
        if value.is_ascii() {
            return ascii_tokenize(&value);
        }
        let simplified = zhconv(&value, Variant::ZhHans);
        let mut tokens = self
            .tokenizer
            .cut(&simplified, false)
            .into_iter()
            .map(str::to_owned)
            .collect::<HashSet<_>>();
        tokens.insert(simplified);
        tokens.into_iter().collect::<Vec<_>>().join(" ")
    }

    fn tokenize(&self, value: &str) -> String {
        if value.is_ascii() {
            return ascii_tokenize(value);
        }
        let simplified = zhconv(&value.replace(['-', '_'], " "), Variant::ZhHans);
        let mut tokens = HashSet::new();
        for word in self.tokenizer.cut(&simplified, false) {
            tokens.insert(word.to_owned());
            let mut initials = String::new();
            let mut full = String::new();
            for pinyin in word.to_pinyin().flatten() {
                initials.push_str(pinyin.first_letter());
                full.push_str(pinyin.plain());
            }
            if !initials.is_empty() {
                tokens.insert(initials);
            }
            if !full.is_empty() {
                tokens.insert(full);
            }
        }
        for pinyin in simplified.as_str().to_pinyin().flatten() {
            tokens.insert(pinyin.first_letter().to_owned());
            tokens.insert(pinyin.plain().to_owned());
        }
        tokens.insert(simplified);
        tokens.into_iter().collect::<Vec<_>>().join(" ")
    }
}

fn ascii_tokenize(value: &str) -> String {
    let raw_lowercase = value.to_lowercase();
    if !value.chars().any(char::is_uppercase) {
        return raw_lowercase;
    }
    let title_lowercase = value.to_case(Case::Title).to_lowercase();
    if title_lowercase == raw_lowercase {
        raw_lowercase
    } else {
        format!("{title_lowercase} {raw_lowercase}")
    }
}

fn bool_bytes(value: bool) -> &'static [u8] {
    if value {
        TRUE_BYTES
    } else {
        FALSE_BYTES
    }
}

fn build_schema() -> Schema {
    let mut builder = Schema::builder();
    let text_options = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default().set_index_option(IndexRecordOption::WithFreqs),
    );
    builder.add_text_field("name", text_options.clone());
    builder.add_text_field("original_name", STORED);
    builder.add_bytes_field("path_key", INDEXED);
    builder.add_bytes_field("path", STORED);
    builder.add_bytes_field("is_log", INDEXED | STORED);
    builder.add_bytes_field("is_archive", INDEXED | STORED);
    builder.add_text_field("ext", text_options);
    builder.build()
}

#[cfg(windows)]
fn path_key(path: &str) -> String {
    path.replace('/', "\\").to_lowercase()
}

#[cfg(not(windows))]
fn path_key(path: &str) -> String {
    path.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn test_dir() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "logcrate-orange-index-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn orange_index_supports_tokens_filters_updates_and_deletes() {
        let directory = test_dir();
        let mut index = SearchIndex::open(&directory).unwrap();
        index
            .add_batch(&[
                SearchIndexEntry {
                    path: "D:\\work\\DataPatchController.log".into(),
                    name: "DataPatchController.log".into(),
                    is_log: true,
                    is_archive: false,
                },
                SearchIndexEntry {
                    path: "C:\\archives\\debug.zip".into(),
                    name: "debug.zip".into(),
                    is_log: false,
                    is_archive: true,
                },
                SearchIndexEntry {
                    path: "D:\\logs\\错误.log".into(),
                    name: "错误.log".into(),
                    is_log: true,
                    is_archive: false,
                },
                SearchIndexEntry {
                    path: "D:\\headers\\log.h".into(),
                    name: "log.h".into(),
                    is_log: false,
                    is_archive: false,
                },
                SearchIndexEntry {
                    path: "C:\\logs\\debug.log".into(),
                    name: "debug.log".into(),
                    is_log: true,
                    is_archive: false,
                },
            ])
            .unwrap();
        index.commit().unwrap();
        let (items, _) = index
            .search(&["data".into(), "patch".into()], "log", 0, 20)
            .unwrap();
        assert_eq!(items[0].path, "D:\\work\\DataPatchController.log");
        let (items, _) = index.search(&["cuowu".into()], "log", 0, 20).unwrap();
        assert_eq!(items[0].path, "D:\\logs\\错误.log");
        let (items, _) = index.search(&["錯誤".into()], "log", 0, 20).unwrap();
        assert_eq!(items[0].path, "D:\\logs\\错误.log");
        let (items, _) = index.search(&["debug.log".into()], "", 0, 20).unwrap();
        assert_eq!(items[0].path, "C:\\logs\\debug.log");
        assert!(items.iter().all(|item| item.name != "log.h"));

        index
            .upsert_batch(&[SearchIndexEntry {
                path: "D:\\work\\DataPatchController.log".into(),
                name: "renamed.log".into(),
                is_log: true,
                is_archive: false,
            }])
            .unwrap();
        index.commit().unwrap();
        assert!(index
            .search(&["datapatchcontroller".into()], "", 0, 20)
            .unwrap()
            .0
            .is_empty());
        index
            .delete_paths(&["D:\\work\\DataPatchController.log".into()])
            .unwrap();
        assert_eq!(index.num_docs(), 4);
        drop(index);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    #[ignore = "requires LOGCRATE_RUNTIME_SEARCH_INDEX pointing to a completed local index"]
    fn runtime_index_returns_cross_volume_debug_log_results() {
        let directory = std::env::var_os("LOGCRATE_RUNTIME_SEARCH_INDEX")
            .map(std::path::PathBuf::from)
            .expect("LOGCRATE_RUNTIME_SEARCH_INDEX is required");
        let index = SearchIndex::open(&directory).unwrap();
        let (items, _) = index.search(&["debug.log".into()], "", 0, 200).unwrap();
        let c_drive = items
            .iter()
            .filter(|item| item.path.starts_with("C:\\"))
            .count();
        let d_drive = items
            .iter()
            .filter(|item| item.path.starts_with("D:\\"))
            .count();
        let exact = items
            .iter()
            .filter(|item| item.name.eq_ignore_ascii_case("debug.log"))
            .count();
        eprintln!("RUNTIME_DEBUG_LOG c={c_drive} d={d_drive} exact={exact}");
        assert!(c_drive > 0, "C drive should contribute debug/log matches");
        assert!(d_drive > 0, "D drive should contribute debug/log matches");
        assert!(exact > 0, "the runtime index should contain debug.log");
    }
}
