//! The title and description each page gives search engines and link
//! previews. `searchTitle` in web/src/state/url.ts builds the same titles in the
//! browser, and `fixtures/page-titles.json` holds both to them.

use std::collections::HashMap;
use std::sync::LazyLock;

use super::{escape_html, search_path, search_path_in_mode, search_query_in_mode, social};
use crate::prewarm::{examples, Example};

const SUFFIX: &str = " · US News Map";
/// The longest title, in characters, suffix included.
const TITLE_LIMIT: usize = 60;
/// The longest description, in characters.
const DESCRIPTION_LIMIT: usize = 160;
/// The most characters of a search's terms a description repeats, leaving
/// room for the sentence around them.
const DESCRIPTION_TERMS_LIMIT: usize = 40;

const PRIVACY: (&str, &str) = (
    "Privacy and data handling · US News Map",
    "US News Map has no accounts, cookies or ads. This page covers page views, the anonymous \
     search log, Cloudflare, Microsoft Azure and how long data is kept.",
);
const DATASETS: (&str, &str) = (
    "Dataset details and index status · US News Map",
    "Newspapers, text sources, languages and places in the US News Map index, with figures for \
     the version currently served and the work in progress.",
);
const CREDITS: (&str, &str) = (
    "OCR engine and model credits · US News Map",
    "The OCR engines and models that supplied replacement text in the US News Map search index, \
     with the author, source and licence of each.",
);

const NEWSPAPERS: &str = "American newspapers from the Library of Congress";
const MAPPED: &str = "mapped by where and when each page was printed";

/// URL parameters that change which pages a search finds. An example's title
/// only fits a URL with its filters or none: the others (the timeline, the
/// map's position, the open tab) show the same search another way.
const FILTERS: [&str; 9] = [
    "source", "mode", "near", "from", "to", "state", "lang", "lccn", "series",
];

/// A page's text, unescaped.
pub(super) struct Head {
    pub(super) title: String,
    pub(super) description: String,
}

impl Head {
    fn fixed((title, description): (&str, &str)) -> Self {
        Self {
            title: title.to_owned(),
            description: description.to_owned(),
        }
    }
}

/// The example searches by canonical path. Examples and the sitemap name a
/// search by that path, so `"Mexican war"` and `/search/mexican-war` are one.
static EXAMPLES: LazyLock<HashMap<String, &'static Example>> = LazyLock::new(|| {
    examples()
        .iter()
        .filter_map(|example| Some((search_path(example.view.get("q")?.as_str()?), example)))
        .collect()
});

/// The text for `path`, with the URL's query `query`. `None` keeps
/// `index.html`'s: the home page's own, and the shell of a path with no page.
pub(super) fn for_page(path: &str, query: &str) -> Option<Head> {
    match path.trim_end_matches('/') {
        "/privacy" => return Some(Head::fixed(PRIVACY)),
        "/datasets" => return Some(Head::fixed(DATASETS)),
        "/credits" => return Some(Head::fixed(CREDITS)),
        _ => {}
    }
    let phrase_mode = !matches!(
        param(query, "mode").as_deref(),
        Some("all" | "any" | "near")
    );
    let q = search_query_in_mode(path, phrase_mode)?;
    let example = EXAMPLES
        .get(&search_path_in_mode(&q, phrase_mode))
        .filter(|example| same_filters(example, query));
    Some(match example {
        Some(example) => for_example(example),
        None => for_search(path, query, &q),
    })
}

fn param(query: &str, name: &str) -> Option<String> {
    form_urlencoded::parse(query.as_bytes())
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

/// A filter's value as the app reads it (`parseView` in web/src/state/url.ts):
/// empty when it's the default or invalid, and lists in one case and order.
fn normalized(key: &str, value: &str) -> String {
    let keep = match key {
        "mode" => matches!(value, "all" | "any" | "near"),
        "near" => value
            .parse::<u8>()
            .is_ok_and(|near| (1..=20).contains(&near) && near != 5),
        "source" => value == "congress",
        "from" | "to" => {
            value.len() == 10 && chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok()
        }
        _ => {
            let mut items: Vec<String> = value
                .split(',')
                .map(|item| item.trim().to_lowercase())
                .filter(|item| !item.is_empty())
                .collect();
            items.sort();
            items.dedup();
            return items.join(",");
        }
    };
    if keep {
        value.to_owned()
    } else {
        String::new()
    }
}

/// Whether a URL's filters are the example's, or absent as in the sitemap.
fn same_filters(example: &Example, query: &str) -> bool {
    let wanted = |key: &str| match example.view.get(key) {
        Some(serde_json::Value::Array(values)) => values
            .iter()
            .filter_map(|value| value.as_str())
            .collect::<Vec<_>>()
            .join(","),
        Some(value) => value.as_str().unwrap_or_default().to_owned(),
        None => String::new(),
    };
    let requested = |key: &str| normalized(key, &param(query, key).unwrap_or_default());
    FILTERS.iter().all(|key| requested(key).is_empty())
        || FILTERS
            .iter()
            .all(|key| requested(key) == normalized(key, &wanted(key)))
}

fn for_example(example: &Example) -> Head {
    let year = |key| Some(example.view.get(key)?.as_str()?.get(..4)?.to_owned());
    let sentence = match (year("from"), year("to")) {
        (Some(from), Some(to)) if from == to => {
            format!("Newspaper pages from {from} on a map and timeline.")
        }
        (Some(from), Some(to)) => {
            format!("Newspaper pages from {from} to {to} on a map and timeline.")
        }
        _ => "Newspaper pages on a map and timeline.".to_owned(),
    };
    Head {
        title: format!("{}{SUFFIX}", example.title),
        description: shorten(&format!("{} {sentence}", example.blurb), DESCRIPTION_LIMIT),
    }
}

fn for_search(path: &str, query: &str, q: &str) -> Head {
    let terms = tidy(q);
    let congress = param(query, "source").as_deref() == Some("congress");
    let lead = if congress {
        "Records of Congress: "
    } else {
        "Newspaper coverage of "
    };
    let title = if terms.is_empty() {
        format!("Newspaper search{SUFFIX}")
    } else {
        let room = TITLE_LIMIT - lead.chars().count() - SUFFIX.chars().count();
        format!("{lead}{}{SUFFIX}", shorten(&terms, room))
    };
    if terms.is_empty() {
        return Head {
            title,
            description: format!("Search digitized {NEWSPAPERS}, {MAPPED}."),
        };
    }
    let shown = shorten(&terms, DESCRIPTION_TERMS_LIMIT);
    let source = if congress {
        "the records of Congress"
    } else {
        NEWSPAPERS
    };
    // A plain search's details say only its match type, the default.
    let details = social::details(path, query).filter(|d| congress || d != "Exact phrase");
    let description = match details {
        Some(details) => format!("Search results for {shown} in {source}. {details}."),
        None => format!("Search results for {shown} in {source}, {MAPPED}."),
    };
    Head {
        title,
        description: shorten(&description, DESCRIPTION_LIMIT),
    }
}

/// `text` on one line: control characters and runs of spaces become a space.
fn tidy(text: &str) -> String {
    let text: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `text` in at most `limit` characters, cut at a word boundary and marked
/// with an ellipsis. A long word with no space in its second half is cut
/// inside the word.
fn shorten(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let room: Vec<char> = text.chars().take(limit.saturating_sub(1)).collect();
    let end = room
        .iter()
        .rposition(|c| c.is_whitespace())
        .filter(|end| *end >= room.len() / 2)
        .unwrap_or(room.len());
    let kept: String = room[..end].iter().collect();
    format!("{}…", kept.trim_end())
}

/// Replaces the first `<meta>` tag naming `name` with one holding `value`,
/// which must already be escaped. The whole tag goes, so a quote or tag-shaped
/// text in a value can't become markup or match a later replacement.
pub(super) fn set_meta(html: &mut String, attribute: &str, name: &str, value: &str) {
    let marker = format!("{attribute}=\"{name}\"");
    let tag = html.match_indices("<meta").find_map(|(start, _)| {
        let end = start + html[start..].find('>')?;
        html[start..=end].contains(&marker).then_some(start..=end)
    });
    if let Some(tag) = tag {
        html.replace_range(tag, &format!("<meta {marker} content=\"{value}\" />"));
    }
}

/// Writes `title` and `description` to every tag that carries them.
pub(super) fn apply(html: &mut String, head: &Head) {
    let title = escape_html(&head.title);
    let description = escape_html(&head.description);
    if let Some(start) = html.find("<title>") {
        if let Some(end) = html[start..].find("</title>") {
            html.replace_range(
                start..start + end + "</title>".len(),
                &format!("<title>{title}</title>"),
            );
        }
    }
    for (attribute, name, value) in [
        ("property", "og:title", &title),
        ("name", "twitter:title", &title),
        ("name", "description", &description),
        ("property", "og:description", &description),
        ("name", "twitter:description", &description),
    ] {
        set_meta(html, attribute, name, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> String {
        std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/index.html"),
        )
        .unwrap()
    }

    fn content<'a>(html: &'a str, marker: &str) -> &'a str {
        let start = html
            .match_indices("<meta")
            .find_map(|(start, _)| {
                let end = start + html[start..].find('>')?;
                html[start..end].contains(marker).then_some(start)
            })
            .unwrap_or_else(|| panic!("no {marker}"));
        let tag = &html[start..start + html[start..].find('>').unwrap()];
        let value = &tag[tag.find("content=").unwrap() + "content=".len() + 1..];
        &value[..value.find('"').unwrap()]
    }

    fn home() -> (String, String) {
        let html = index();
        let title = &html[html.find("<title>").unwrap() + "<title>".len()..];
        (
            title[..title.find("</title>").unwrap()].to_owned(),
            content(&html, r#"name="description""#).to_owned(),
        )
    }

    fn example(id: &str) -> &'static Example {
        examples().iter().find(|e| e.id == id).unwrap()
    }

    fn path_of(example: &Example) -> String {
        search_path(example.view["q"].as_str().unwrap())
    }

    fn view_query(example: &Example) -> String {
        let mut query = form_urlencoded::Serializer::new(String::new());
        for (key, value) in &example.view {
            if key != "q" {
                let value = match value {
                    serde_json::Value::Array(values) => values
                        .iter()
                        .map(|v| v.as_str().unwrap())
                        .collect::<Vec<_>>()
                        .join(","),
                    value => value.as_str().unwrap().to_owned(),
                };
                query.append_pair(key, &value);
            }
        }
        query.finish()
    }

    #[test]
    fn fixed_pages_are_distinct_and_in_range() {
        let (title, description) = home();
        let fixed = [
            (title.as_str(), description.as_str()),
            PRIVACY,
            DATASETS,
            CREDITS,
        ];
        for (title, description) in fixed {
            assert!(title.contains("US News Map"), "{title}");
            assert!((30..=60).contains(&title.chars().count()), "{title}");
            assert!(
                (110..=160).contains(&description.chars().count()),
                "{description}"
            );
        }
        for (i, (title, description)) in fixed.iter().enumerate() {
            for (other_title, other_description) in &fixed[i + 1..] {
                assert_ne!(title, other_title);
                assert_ne!(description, other_description);
            }
        }
    }

    #[test]
    fn an_example_page_uses_the_examples_own_words() {
        let war = example("mexican-war");
        let head = for_page(&path_of(war), "").unwrap();
        assert_eq!(head.title, "Mexican-American War · US News Map");
        assert_eq!(
            head.description,
            "The war of 1846 to 1848, which ended with Mexico ceding the Southwest. \
             Newspaper pages from 1846 to 1849 on a map and timeline."
        );
        // The link an example card opens is the same example.
        let opened = for_page(&path_of(war), &view_query(war)).unwrap();
        assert_eq!(opened.title, head.title);
        // The map's position and the timeline don't change the search.
        let moved = for_page(&path_of(war), "t=1847-01-01&z=4&utm_source=x").unwrap();
        assert_eq!(moved.title, head.title);
    }

    #[test]
    fn a_page_for_one_year_names_the_year_once() {
        let single = examples()
            .iter()
            .find(|e| e.view["from"].as_str().unwrap()[..4] == e.view["to"].as_str().unwrap()[..4])
            .unwrap();
        let year = &single.view["from"].as_str().unwrap()[..4];
        let head = for_page(&path_of(single), "").unwrap();
        assert!(head.description.ends_with(&format!(
            "Newspaper pages from {year} on a map and timeline."
        )));
    }

    #[test]
    fn an_example_with_a_list_filter_matches_its_list() {
        let krieg = example("krieg-1870");
        let path = path_of(krieg);
        assert_eq!(
            for_page(&path, &view_query(krieg)).unwrap().title,
            format!("{} · US News Map", krieg.title)
        );
        assert_eq!(
            for_page(&path, "from=1870-01-01&to=1871-12-31&lang=GER,ger")
                .unwrap()
                .title,
            format!("{} · US News Map", krieg.title)
        );
        assert_eq!(
            for_page(&path, "lang=fre").unwrap().title,
            "Newspaper coverage of krieg · US News Map"
        );
    }

    #[test]
    fn other_filters_or_a_partial_view_are_not_the_example() {
        let war = example("mexican-war");
        let path = path_of(war);
        for query in [
            "from=1846-01-01",
            "state=GA",
            "from=1846-01-01&to=1849-12-31&state=GA",
            "from=1900-01-01&to=1910-12-31",
            "source=congress",
            "mode=near&near=3",
        ] {
            let head = for_page(&path, query).unwrap();
            assert_ne!(head.title, "Mexican-American War · US News Map", "{query}");
        }
        // Filters the app would ignore or normalize are the same search.
        for query in [
            "from=not-a-date",
            "mode=bogus",
            "near=5",
            "source=news",
            "from=1846-01-01&to=1849-12-31&state=",
        ] {
            let head = for_page(&path, query).unwrap();
            assert_eq!(head.title, "Mexican-American War · US News Map", "{query}");
        }
        // Naming the default match type is the same search.
        assert_eq!(
            for_page(&path, "mode=phrase").unwrap().title,
            "Mexican-American War · US News Map"
        );
    }

    #[test]
    fn any_other_search_gets_a_generated_title_and_description() {
        let head = for_page("/search/old-glory", "").unwrap();
        assert_eq!(head.title, "Newspaper coverage of old glory · US News Map");
        assert_eq!(
            head.description,
            "Search results for old glory in American newspapers from the Library of \
             Congress, mapped by where and when each page was printed."
        );
    }

    #[test]
    fn a_filtered_search_keeps_its_filters_in_the_description() {
        let head = for_page("/search/gold", "from=1896-01-01&state=GA").unwrap();
        assert_eq!(head.title, "Newspaper coverage of gold · US News Map");
        assert_eq!(
            head.description,
            "Search results for gold in American newspapers from the Library of Congress. \
             1896-01-01 to latest | Exact phrase | States: GA."
        );
    }

    #[test]
    fn a_congress_search_does_not_claim_to_be_newspapers() {
        let head = for_page("/search/tariff", "source=congress").unwrap();
        assert_eq!(head.title, "Records of Congress: tariff · US News Map");
        assert!(!head.description.contains("newspapers"));
        assert!(head.description.contains("the records of Congress"));
    }

    #[test]
    fn blank_terms_still_produce_a_page() {
        let head = for_page("/search/%20", "").unwrap();
        assert_eq!(head.title, "Newspaper search · US News Map");
        assert!((110..=DESCRIPTION_LIMIT).contains(&head.description.chars().count()));
    }

    #[test]
    fn long_text_is_cut_at_a_word_boundary_and_stays_valid() {
        let long = ["gold rush"; 20].join(" ");
        let head = for_page(&search_path(&long), "").unwrap();
        assert_eq!(
            head.title,
            "Newspaper coverage of gold rush gold rush… · US News Map"
        );
        assert!(head
            .description
            .contains("rush gold… in American newspapers"));
        assert!(head.description.chars().count() <= DESCRIPTION_LIMIT);
        // One unbroken word has no boundary to use.
        let word = "x".repeat(256);
        let head = for_page(&search_path(&word), "").unwrap();
        assert_eq!(head.title.chars().count(), TITLE_LIMIT);
        assert!(head.description.chars().count() <= DESCRIPTION_LIMIT);
        // Filters past the limit are cut too, and stay in characters.
        let head = for_page("/search/gold", &format!("state={}", "é".repeat(256))).unwrap();
        assert!(head.description.chars().count() <= DESCRIPTION_LIMIT);
        assert!(head.description.ends_with('…'));
    }

    #[test]
    fn non_ascii_terms_are_counted_in_characters() {
        let head = for_page("/search/%EA%AE%B3%EA%AE%83%EA%AD%B9", "").unwrap();
        assert_eq!(head.title, "Newspaper coverage of ꮳꮃꭹ · US News Map");
        let long = "日本語".repeat(80);
        let head = for_page(&search_path(&long), "").unwrap();
        assert_eq!(head.title.chars().count(), TITLE_LIMIT);
        assert!(head.description.contains("日本語"));
        assert_eq!(shorten("héllo wörld", 8), "héllo…");
    }

    #[test]
    fn control_characters_never_reach_the_page() {
        let head = for_page("/search/a%0Ab%00c%20%20d", "").unwrap();
        assert_eq!(head.title, "Newspaper coverage of a b c d · US News Map");
    }

    #[test]
    fn shorten_leaves_text_that_fits_alone() {
        assert_eq!(shorten("gold", 4), "gold");
        assert_eq!(shorten("gold rush", 5), "gold…");
        assert_eq!(shorten("goldrush", 5), "gold…");
    }

    #[test]
    fn every_tag_carries_the_same_text_and_tag_shaped_terms_stay_text() {
        let terms = r#"</title><script>"'&"#;
        let mut html = index();
        apply(&mut html, &for_page(&search_path(terms), "").unwrap());
        let title =
            "Newspaper coverage of &lt;/title&gt;&lt;script&gt;&quot;&#39;&amp; · US News Map";
        assert!(html.contains(&format!("<title>{title}</title>")));
        assert_eq!(content(&html, r#"property="og:title""#), title);
        assert_eq!(content(&html, r#"name="twitter:title""#), title);
        let description = content(&html, r#"name="description""#);
        assert!(description.starts_with("Search results for &lt;/title&gt;"));
        assert_eq!(content(&html, r#"property="og:description""#), description);
        assert_eq!(content(&html, r#"name="twitter:description""#), description);
        assert!(!html.contains("<script>\""));
    }

    /// The sitemap holds the static pages, the examples, and searches promoted
    /// from the search log; the last are generated like any other search.
    #[test]
    fn the_sitemaps_pages_have_unique_titles_and_descriptions() {
        let promoted = [
            "\"gold diggings\" OR \"gold mines\" OR \"gold region\"",
            "ꮳꮃꭹ",
            "abolitionists",
            "merrimac AND monitor",
            "fourteen points",
            "john brown",
            "titanic",
        ];
        let mut pages: Vec<(String, Head)> = ["/privacy", "/datasets", "/credits"]
            .into_iter()
            .map(|path| (path.to_owned(), for_page(path, "").unwrap()))
            .collect();
        let (title, description) = home();
        pages.push(("/".to_owned(), Head { title, description }));
        for path in crate::site::CURATED_SEARCHES
            .iter()
            .cloned()
            .chain(promoted.into_iter().map(search_path))
        {
            let head = for_page(&path, "").unwrap();
            pages.push((path, head));
        }
        assert!(pages.len() > examples().len());
        let mut titles = std::collections::HashSet::new();
        let mut descriptions = std::collections::HashSet::new();
        for (path, head) in &pages {
            assert!(titles.insert(&head.title), "{path}: {}", head.title);
            assert!(descriptions.insert(&head.description), "{path}");
            assert!(head.title.chars().count() <= TITLE_LIMIT, "{path}");
            assert!(
                (110..=DESCRIPTION_LIMIT).contains(&head.description.chars().count()),
                "{path}: {}",
                head.description
            );
        }
    }

    /// The browser builds the same titles (web/src/pageTitle.test.ts reads this
    /// file too), so a page doesn't change its title when the app starts.
    #[test]
    fn titles_match_the_shared_cases() {
        #[derive(serde::Deserialize)]
        struct Case {
            title: String,
            url: String,
        }
        let cases: Vec<Case> = serde_json::from_str(
            &std::fs::read_to_string(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../fixtures/page-titles.json"),
            )
            .unwrap(),
        )
        .unwrap();
        for case in cases {
            let (path, query) = case.url.split_once('?').unwrap_or((&case.url, ""));
            let title = match for_page(path, query) {
                Some(head) => head.title,
                None => home().0,
            };
            assert_eq!(title, case.title, "{}", case.url);
        }
    }
}
