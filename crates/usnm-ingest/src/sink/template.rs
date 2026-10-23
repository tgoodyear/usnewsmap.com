//! The index config a run creates: the template with the writer's tuning, the decade
//! layout, the text fields, the page languages and the split target (05 §5.5).

use anyhow::bail;

use usnm_core::text_layout::TextLayout;

use super::{INDEX_TEMPLATE, INGEST_QUEUE_MEMORY};
use crate::merges;

/// How hard the writer may work for one run (#172): Quickwit's indexing heap
/// and commit timeout for the new index, and the node's in-memory ingest
/// queue. The defaults are the index template's and [`INGEST_QUEUE_MEMORY`],
/// sized for the Consumption profile's 7.5 GiB container. On the E4 profile
/// (26 GiB, a full rebuild) the ingest job sets more through
/// `USNM_WRITER_HEAP`, `USNM_WRITER_COMMIT_SECS` and `USNM_WRITER_QUEUE`: with
/// a heap that holds a whole 30,000-page split and a commit timeout longer than
/// it takes to index one, the indexer writes splits at the target size, so
/// they need little merging. At 30 s, a full rebuild's indexer cut splits of
/// about 7,000 pages, and merging them cost the writer its memory and much of
/// its CPU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriterTuning {
    pub heap: String,
    pub commit_timeout_secs: u32,
    pub queue: String,
}

/// How a main index lays out its pages by decade (05 §5.5.5, #123). Only
/// this run's new main index takes it; the Japanese index never does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Decades {
    /// No `decade` field: the template as it is.
    #[default]
    Off,
    /// The `decade` field, recorded as a split tag (`tag_fields`): a delta
    /// on a version laid out by decade. Its splits mix decades as before,
    /// but a search skips each split that holds none of its decades.
    Tagged,
    /// Also `partition_key: decade`, so every split holds one decade: a
    /// full base built with `--partition-decade`.
    Partitioned,
}

/// The template lines [`Decades`] adds to: the doc mapping's timestamp
/// field and the `year` field mapping.
const TEMPLATE_TIMESTAMP: &str = "timestamp_field: date";
const TEMPLATE_YEAR: &str = "- { name: year, type: u64, fast: true, indexed: true }";

/// Partitions an indexer keeps apart at once, more than the 15 decades
/// (1820s to 1960s, `usnm_core::decade`). Past it, Quickwit 0.9.1 puts the
/// pages of further decades into one shared split, which a search can't
/// skip. Each partition has its own in-memory split while the indexer
/// fills, all within the one indexing heap.
pub const MAX_NUM_PARTITIONS: u32 = 32;

impl Decades {
    /// Whether the pages carry the `decade` field.
    pub fn on(self) -> bool {
        self != Decades::Off
    }

    /// `template` with the `decade` field, and its tag and partitioning as
    /// this layout has them. `Off` leaves it as it is. Each line it adds to
    /// must be there exactly once, so a template change can't silently
    /// drop the layout.
    pub fn apply(self, template: &str) -> anyhow::Result<String> {
        if self == Decades::Off {
            return Ok(template.to_owned());
        }
        let mut found = (0, 0);
        let mut out = Vec::new();
        for line in template.lines() {
            let indent = &line[..line.len() - line.trim_start().len()];
            out.push(line.to_owned());
            match line.trim() {
                TEMPLATE_TIMESTAMP => {
                    found.0 += 1;
                    out.push(format!(
                        "{indent}# Decade partitions (05 §5.5.5, #123): searches name their \
                         decades (`decade:IN [...]`), and Quickwit skips the splits of the others."
                    ));
                    out.push(format!("{indent}tag_fields: [decade]"));
                    if self == Decades::Partitioned {
                        out.push(format!("{indent}partition_key: decade"));
                        out.push(format!("{indent}max_num_partitions: {MAX_NUM_PARTITIONS}"));
                    }
                }
                TEMPLATE_YEAR => {
                    found.1 += 1;
                    out.push(format!(
                        "{indent}# The page's decade, from 1820 (usnm_core::decade::of_year)."
                    ));
                    out.push(format!(
                        "{indent}- {{ name: decade, type: u64, fast: false, indexed: true, stored: false }}"
                    ));
                }
                _ => {}
            }
        }
        if found != (1, 1) {
            bail!(
                "the index template needs exactly one `{TEMPLATE_TIMESTAMP}` and one `{TEMPLATE_YEAR}` line to add the decade to, not {} and {}",
                found.0,
                found.1
            );
        }
        let mut out = out.join("\n");
        if template.ends_with('\n') {
            out.push('\n');
        }
        Ok(out)
    }
}

/// How this run's new main index lays its pages out: by decade (05
/// §5.5.5), in which text fields (05 §5.5.7), with the page languages or
/// not (05 §5.5.8) and in splits of how many pages. Only the main index
/// takes it; the Japanese index keeps its own mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MainLayout {
    pub decades: Decades,
    pub text: TextLayout,
    /// The `page_language` and `page_language_set` fields.
    pub page_language: bool,
    /// The split target in place of the template's (`--split-docs`, #283):
    /// only a full base takes one. `None` keeps the template's.
    pub split_docs: Option<u64>,
}

impl From<Decades> for MainLayout {
    fn from(decades: Decades) -> Self {
        MainLayout {
            decades,
            ..MainLayout::default()
        }
    }
}

impl MainLayout {
    /// The pages a merge stops at in this index: `split_docs`, or the
    /// template's `split_num_docs_target`.
    pub fn split_target(&self) -> u64 {
        self.split_docs
            .unwrap_or_else(merges::template_split_target)
    }
}

/// The main index's config for this run: the template with the writer's
/// tuning, the split target, the decade layout, the text fields and the
/// page languages.
pub fn main_template(tuning: &WriterTuning, layout: MainLayout) -> anyhow::Result<String> {
    let mut template = text_fields(layout.text, &tuning.apply(INDEX_TEMPLATE)?)?;
    if let Some(docs) = layout.split_docs {
        template = split_target(docs, &template)?;
    }
    let template = page_language_fields(layout.page_language, &template)?;
    layout.decades.apply(&template)
}

/// The template line the page-language fields follow: the title languages.
const TEMPLATE_LANGUAGE: &str =
    "- { name: language, type: array<text>, tokenizer: raw, fast: true }";

/// `template` with the page-language fields (05 §5.5.8, #289) after the
/// title languages' when `on`; as it is otherwise. That line must be there
/// exactly once, so a template change can't silently drop the fields.
pub fn page_language_fields(on: bool, template: &str) -> anyhow::Result<String> {
    use usnm_core::page_language::{FIELD, SET_FIELD};
    if !on {
        return Ok(template.to_owned());
    }
    let mut found = 0;
    let mut out = Vec::new();
    for line in template.lines() {
        let indent = &line[..line.len() - line.trim_start().len()];
        out.push(line.to_owned());
        if line.trim() == TEMPLATE_LANGUAGE {
            found += 1;
            out.push(format!(
                "{indent}# The page's own languages, found from its words (05 §5.5.8, #289): \
                 `lang=` matches them"
            ));
            out.push(format!(
                "{indent}# and the counts by language read them. Sorted; a mixed page has two."
            ));
            out.push(format!(
                "{indent}- {{ name: {FIELD}, type: array<text>, tokenizer: raw, fast: true }}"
            ));
            out.push(format!(
                "{indent}# The same languages as one keyword (`eng_ger`, `none`): the filter's \
                 complement side."
            ));
            out.push(format!(
                "{indent}- {{ name: {SET_FIELD}, type: text, tokenizer: raw, indexed: true, \
                 stored: false, fast: false }}"
            ));
        }
    }
    if found != 1 {
        bail!(
            "the index template needs exactly one `{TEMPLATE_LANGUAGE}` line to add the page \
             languages after, not {found}"
        );
    }
    let mut out = out.join("\n");
    if template.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

/// The template line [`split_target`] replaces.
const TEMPLATE_SPLIT_TARGET: &str = "split_num_docs_target: 30000";

/// The split targets [`split_target`] takes are below this. A
/// million-page split (about 23 GB before the common-word pairs) killed
/// the writer while it uploaded in October 2026 (08 §8.4).
pub const MAX_SPLIT_DOCS: u64 = 1_000_000;

/// `template` with `split_num_docs_target: docs` (#283): a merge stops
/// taking splits once it holds that many pages, so a larger target leaves
/// fewer, larger splits. The line must be there exactly once, so a
/// template change can't silently keep the template's target.
pub fn split_target(docs: u64, template: &str) -> anyhow::Result<String> {
    if docs == 0 || docs >= MAX_SPLIT_DOCS {
        bail!("the split target must be above 0 and below {MAX_SPLIT_DOCS}, not {docs}");
    }
    let found = template
        .lines()
        .filter(|l| l.trim() == TEMPLATE_SPLIT_TARGET)
        .count();
    if found != 1 {
        bail!("the index template needs exactly one `{TEMPLATE_SPLIT_TARGET}` line, not {found}");
    }
    let mut out = template
        .lines()
        .map(|l| {
            if l.trim() == TEMPLATE_SPLIT_TARGET {
                let indent = &l[..l.len() - l.trim_start().len()];
                format!("{indent}split_num_docs_target: {docs}")
            } else {
                l.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    if template.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

/// The template's text field mappings for [`TextLayout::Separate`], in the
/// order they appear: LoC's text and pairs, American Stories' text and pairs.
const TEMPLATE_TEXT_FIELDS: [&str; 4] = [
    "- { name: text, type: text, tokenizer: usnm_text, record: position, stored: true, fieldnorms: false }",
    "- { name: text_cg, type: text, tokenizer: whitespace, record: position, stored: false, fieldnorms: false }",
    "- { name: text_as, type: text, tokenizer: usnm_text, record: position, stored: true, fieldnorms: false }",
    "- { name: text_as_cg, type: text, tokenizer: whitespace, record: position, stored: false, fieldnorms: false }",
];
const TEMPLATE_SEARCH_FIELDS: &str = "default_search_fields: [text]";

/// `template` with the text fields of `layout` (05 §5.5.7). `Separate`
/// leaves it as it is. `Single` keeps `text` and `text_as` stored but not
/// indexed, drops their pairs, and adds `text_all` and `text_all_cg`, each
/// with the comment lines above it replaced. Each line it changes must be
/// there exactly once, so a template change can't silently keep a searched
/// field of one text.
pub fn text_fields(layout: TextLayout, template: &str) -> anyhow::Result<String> {
    if layout == TextLayout::Separate {
        return Ok(template.to_owned());
    }
    let mut found = [0usize; 5];
    let mut out: Vec<String> = Vec::new();
    for line in template.lines() {
        let indent = &line[..line.len() - line.trim_start().len()];
        let trimmed = line.trim();
        if let Some(i) = TEMPLATE_TEXT_FIELDS.iter().position(|f| *f == trimmed) {
            found[i] += 1;
            // The comment above the field described its old role.
            while out
                .last()
                .is_some_and(|l| l.trim_start().starts_with('#') && l.starts_with(indent))
            {
                out.pop();
            }
            if i == 0 {
                for l in [
                    "# One searched field for both texts (05 §5.5.7, #283). LoC's text (or",
                    "# our OCR's Latin text, #203) and American Stories' (05 §5.5.4) are",
                    "# stored for snippets and `matched_in`, not searched.",
                    "- { name: text, type: text, indexed: false, stored: true }",
                    "- { name: text_as, type: text, indexed: false, stored: true }",
                    "# Both texts' words, one token per word as the release folds them,",
                    "# with 32 positions no query can match between the texts",
                    "# (usnm_core::text_layout::index_fields), and their common-word",
                    "# pairs (05 §5.5.3), made separately for each text.",
                    "- { name: text_all, type: text, tokenizer: whitespace, record: position, stored: false, fieldnorms: false }",
                    "- { name: text_all_cg, type: text, tokenizer: whitespace, record: position, stored: false, fieldnorms: false }",
                ] {
                    out.push(format!("{indent}{l}"));
                }
            }
        } else if trimmed == TEMPLATE_SEARCH_FIELDS {
            found[4] += 1;
            out.push(format!(
                "{indent}default_search_fields: [{}]",
                usnm_core::text_layout::FIELD
            ));
        } else {
            out.push(line.to_owned());
        }
    }
    if found != [1; 5] {
        bail!(
            "the index template needs each of its four text fields and `{TEMPLATE_SEARCH_FIELDS}` \
             exactly once to lay them out in one field, not {found:?}"
        );
    }
    let mut out = out.join("\n");
    if template.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

/// The template lines [`WriterTuning`] replaces.
const TEMPLATE_HEAP: &str = "heap_size: 1GiB";
const TEMPLATE_COMMIT: &str = "commit_timeout_secs: 30";

impl Default for WriterTuning {
    fn default() -> Self {
        WriterTuning {
            heap: "1GiB".into(),
            commit_timeout_secs: 30,
            queue: INGEST_QUEUE_MEMORY.into(),
        }
    }
}

impl WriterTuning {
    /// From the environment, the defaults for what isn't set.
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_lookup(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let d = WriterTuning::default();
        // A whole number of MiB or GiB above zero, with exactly one unit.
        let size = |key: &str, default: String| -> anyhow::Result<String> {
            let Some(v) = get(key) else {
                return Ok(default);
            };
            let amount = v.strip_suffix("GiB").or_else(|| v.strip_suffix("MiB"));
            match amount {
                Some(n)
                    if n.bytes().all(|b| b.is_ascii_digit())
                        && n.parse::<u64>().is_ok_and(|n| n > 0) =>
                {
                    Ok(v)
                }
                _ => bail!("{key} must be a size like 6GiB or 512MiB, not `{v}`"),
            }
        };
        let commit_timeout_secs = match get("USNM_WRITER_COMMIT_SECS") {
            None => d.commit_timeout_secs,
            Some(v) => match v.parse::<u32>() {
                Ok(n) if (1..=3600).contains(&n) => n,
                _ => bail!("USNM_WRITER_COMMIT_SECS must be 1 to 3600 seconds, not `{v}`"),
            },
        };
        Ok(WriterTuning {
            heap: size("USNM_WRITER_HEAP", d.heap)?,
            commit_timeout_secs,
            queue: size("USNM_WRITER_QUEUE", d.queue)?,
        })
    }

    /// `template` with this run's heap and commit timeout. Each must be an
    /// active setting line (not a comment) and there must be exactly one of
    /// each, so a template change can't silently drop the tuning.
    pub fn apply(&self, template: &str) -> anyhow::Result<String> {
        let mut found = (0, 0);
        let lines: Vec<String> = template
            .lines()
            .map(|line| {
                let indent = &line[..line.len() - line.trim_start().len()];
                match line.trim() {
                    TEMPLATE_HEAP => {
                        found.0 += 1;
                        format!("{indent}heap_size: {}", self.heap)
                    }
                    TEMPLATE_COMMIT => {
                        found.1 += 1;
                        format!("{indent}commit_timeout_secs: {}", self.commit_timeout_secs)
                    }
                    _ => line.to_owned(),
                }
            })
            .collect();
        if found != (1, 1) {
            bail!(
                "the index template needs exactly one `{TEMPLATE_HEAP}` and one `{TEMPLATE_COMMIT}` setting to tune, not {} and {}",
                found.0,
                found.1
            );
        }
        let mut out = lines.join("\n");
        if template.ends_with('\n') {
            out.push('\n');
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_tuning_defaults_and_overrides() {
        let none = |_: &str| None;
        assert_eq!(
            WriterTuning::from_lookup(none).unwrap(),
            WriterTuning::default()
        );
        // The defaults leave the template as it is.
        assert_eq!(
            WriterTuning::default().apply(INDEX_TEMPLATE).unwrap(),
            INDEX_TEMPLATE
        );
        let e4 = |k: &str| match k {
            "USNM_WRITER_HEAP" => Some("6GiB".to_string()),
            "USNM_WRITER_COMMIT_SECS" => Some("600".to_string()),
            "USNM_WRITER_QUEUE" => Some("4GiB".to_string()),
            _ => None,
        };
        let t = WriterTuning::from_lookup(e4).unwrap();
        assert_eq!(
            (t.heap.as_str(), t.commit_timeout_secs, t.queue.as_str()),
            ("6GiB", 600, "4GiB")
        );
        let yaml = t.apply(INDEX_TEMPLATE).unwrap();
        assert!(yaml.contains("heap_size: 6GiB") && yaml.contains("commit_timeout_secs: 600"));
        assert!(!yaml.contains("heap_size: 1GiB") && !yaml.contains("commit_timeout_secs: 30"));
        // Everything else is the template's.
        assert_eq!(yaml.lines().count(), INDEX_TEMPLATE.lines().count());
    }

    #[test]
    fn decade_layouts_add_the_field_and_its_tag_or_partitions() {
        assert_eq!(Decades::Off.apply(INDEX_TEMPLATE).unwrap(), INDEX_TEMPLATE);
        let tagged = Decades::Tagged.apply(INDEX_TEMPLATE).unwrap();
        let partitioned = Decades::Partitioned.apply(INDEX_TEMPLATE).unwrap();
        for yaml in [&tagged, &partitioned] {
            assert!(yaml.contains("\n  tag_fields: [decade]\n"), "{yaml}");
            assert!(yaml.contains(
                "\n    - { name: decade, type: u64, fast: false, indexed: true, stored: false }\n"
            ));
            // Only the doc mapping changes: the indexing settings, which the
            // merge checks read, are the template's.
            let settings = |t: &str| t[t.find("indexing_settings:").unwrap()..].to_owned();
            assert_eq!(settings(yaml), settings(INDEX_TEMPLATE));
            // The template's lines, in order, with the new ones between them.
            let mut lines = yaml.lines();
            assert!(INDEX_TEMPLATE.lines().all(|l| lines.any(|y| y == l)));
        }
        assert!(!tagged.contains("partition_key"));
        assert!(partitioned.contains("\n  partition_key: decade\n  max_num_partitions: 32\n"));
        // With the writer's tuning too.
        let t = WriterTuning {
            heap: "6GiB".into(),
            commit_timeout_secs: 120,
            queue: "4GiB".into(),
        };
        let yaml = main_template(&t, Decades::Partitioned.into()).unwrap();
        assert!(yaml.contains("heap_size: 6GiB") && yaml.contains("partition_key: decade"));
        // A template without the lines it adds to is refused.
        assert!(Decades::Tagged.apply("version: 0.9").is_err());
        let twice = format!("{INDEX_TEMPLATE}\n  timestamp_field: date\n");
        assert!(Decades::Partitioned.apply(&twice).is_err());
    }

    #[test]
    fn one_text_field_keeps_the_texts_stored_and_searches_text_all() {
        assert_eq!(
            text_fields(TextLayout::Separate, INDEX_TEMPLATE).unwrap(),
            INDEX_TEMPLATE
        );
        let yaml = text_fields(TextLayout::Single, INDEX_TEMPLATE).unwrap();
        let fields: Vec<&str> = yaml
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with("- { name: text"))
            .collect();
        assert_eq!(
            fields,
            [
                "- { name: text, type: text, indexed: false, stored: true }",
                "- { name: text_as, type: text, indexed: false, stored: true }",
                "- { name: text_all, type: text, tokenizer: whitespace, record: position, stored: false, fieldnorms: false }",
                "- { name: text_all_cg, type: text, tokenizer: whitespace, record: position, stored: false, fieldnorms: false }",
            ]
        );
        assert!(
            yaml.contains("\n  default_search_fields: [text_all]"),
            "{yaml}"
        );
        assert!(!yaml.contains("default_search_fields: [text]"));
        // The comments about the old fields went with them.
        assert!(!yaml.contains("Searched only when"), "{yaml}");
        assert!(!yaml.contains("Phrases holding a common"), "{yaml}");
        // Everything but the text fields is the template's.
        let settings = |t: &str| t[t.find("    - { name: date").unwrap()..].to_owned();
        assert_eq!(
            settings(&yaml).replace("[text_all]", "[text]"),
            settings(INDEX_TEMPLATE)
        );
        assert!(
            yaml.starts_with(&INDEX_TEMPLATE[..INDEX_TEMPLATE.find("field_mappings:").unwrap()])
        );
        // Strict, as every main index: a document with a field of the other
        // layout (`text_cg` here, `text_all` in the template) is refused.
        assert!(yaml.contains("\n  mode: strict\n"));
        assert!(!INDEX_TEMPLATE.contains("name: text_all"));
        // With the writer's tuning, a decade layout and a split target too.
        let t = WriterTuning {
            heap: "6GiB".into(),
            commit_timeout_secs: 120,
            queue: "4GiB".into(),
        };
        let layout = MainLayout {
            decades: Decades::Partitioned,
            text: TextLayout::Single,
            page_language: false,
            split_docs: Some(120_000),
        };
        let yaml = main_template(&t, layout).unwrap();
        assert!(yaml.contains("\n  split_num_docs_target: 120000\n"));
        assert!(yaml.contains("heap_size: 6GiB") && yaml.contains("partition_key: decade"));
        assert!(yaml.contains("name: text_all,") && yaml.contains("name: decade,"));
        // A template without one of the lines it changes is refused.
        let no_pairs = INDEX_TEMPLATE.replace(TEMPLATE_TEXT_FIELDS[1], "");
        assert!(text_fields(TextLayout::Single, &no_pairs).is_err());
        assert!(text_fields(TextLayout::Single, "version: 0.9").is_err());
    }

    #[test]
    fn page_languages_add_two_keyword_fields() {
        assert_eq!(
            page_language_fields(false, INDEX_TEMPLATE).unwrap(),
            INDEX_TEMPLATE
        );
        let yaml = page_language_fields(true, INDEX_TEMPLATE).unwrap();
        let fields: Vec<&str> = yaml
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with("- { name: page_language") || *l == TEMPLATE_LANGUAGE)
            .collect();
        assert_eq!(
            fields,
            [
                TEMPLATE_LANGUAGE,
                "- { name: page_language, type: array<text>, tokenizer: raw, fast: true }",
                "- { name: page_language_set, type: text, tokenizer: raw, indexed: true, stored: false, fast: false }",
            ]
        );
        // The template's lines, in order, with the new ones between them.
        let mut lines = yaml.lines();
        assert!(INDEX_TEMPLATE.lines().all(|l| lines.any(|y| y == l)));
        assert_eq!(yaml.lines().count(), INDEX_TEMPLATE.lines().count() + 5);
        // With every other layout.
        let t = WriterTuning {
            heap: "6GiB".into(),
            commit_timeout_secs: 120,
            queue: "4GiB".into(),
        };
        let layout = MainLayout {
            decades: Decades::Partitioned,
            text: TextLayout::Single,
            page_language: true,
            split_docs: Some(120_000),
        };
        let yaml = main_template(&t, layout).unwrap();
        for field in [
            "name: text_all,",
            "name: decade,",
            "name: page_language_set,",
            "split_num_docs_target: 120000\n",
        ] {
            assert!(yaml.contains(field), "{field}");
        }
        assert!(page_language_fields(true, "version: 0.9").is_err());
    }

    #[test]
    fn a_split_target_replaces_the_templates() {
        let default = MainLayout::default();
        assert_eq!(default.split_target(), 30_000);
        assert_eq!(
            main_template(&WriterTuning::default(), default).unwrap(),
            INDEX_TEMPLATE
        );
        let layout = MainLayout {
            split_docs: Some(90_000),
            ..MainLayout::default()
        };
        assert_eq!(layout.split_target(), 90_000);
        let yaml = main_template(&WriterTuning::default(), layout).unwrap();
        assert_eq!(
            yaml,
            INDEX_TEMPLATE.replace(
                "  split_num_docs_target: 30000\n",
                "  split_num_docs_target: 90000\n"
            )
        );
        assert!(split_target(0, INDEX_TEMPLATE).is_err());
        assert!(split_target(MAX_SPLIT_DOCS, INDEX_TEMPLATE).is_err());
        // A template without the line, or with it twice, is refused.
        assert!(split_target(90_000, "version: 0.9").is_err());
        let twice = format!("{INDEX_TEMPLATE}  {TEMPLATE_SPLIT_TARGET}\n");
        assert!(split_target(90_000, &twice).is_err());
    }

    #[test]
    fn writer_tuning_refuses_bad_values_and_untunable_templates() {
        for (k, v) in [
            ("USNM_WRITER_HEAP", "6"),
            ("USNM_WRITER_HEAP", "6GB"),
            ("USNM_WRITER_HEAP", "0GiB"),
            ("USNM_WRITER_HEAP", "00GiB"),
            ("USNM_WRITER_HEAP", "6GiBGiB"),
            ("USNM_WRITER_HEAP", "6GiBMiB"),
            ("USNM_WRITER_HEAP", "GiB"),
            ("USNM_WRITER_HEAP", "-6GiB"),
            ("USNM_WRITER_QUEUE", "lots"),
            ("USNM_WRITER_COMMIT_SECS", "0"),
            ("USNM_WRITER_COMMIT_SECS", "7200"),
            ("USNM_WRITER_COMMIT_SECS", "ten"),
        ] {
            let got = WriterTuning::from_lookup(|key| (key == k).then(|| v.to_string()));
            assert!(got.is_err(), "{k}={v}");
        }
        assert!(WriterTuning::default().apply("version: 0.9").is_err());
        // A commented-out setting doesn't count, and neither do two.
        let commented = INDEX_TEMPLATE.replace("heap_size: 1GiB", "# heap_size: 1GiB");
        assert!(WriterTuning::default().apply(&commented).is_err());
        let twice = format!("{INDEX_TEMPLATE}\n  commit_timeout_secs: 30\n");
        assert!(WriterTuning::default().apply(&twice).is_err());
    }
}
