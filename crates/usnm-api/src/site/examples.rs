//! Stable example routes share the presets used by the browser and cache warm-up.

use crate::prewarm::{examples, Example};

pub(super) fn path(example: &Example) -> String {
    format!("/examples/{}", example.id)
}

pub(super) fn for_path(path: &str) -> Option<&'static Example> {
    let id = path.strip_prefix("/examples/")?;
    let id = id.strip_suffix('/').unwrap_or(id);
    examples().iter().find(|example| example.id == id)
}

pub(super) fn view_query(example: &Example, query: &str) -> String {
    let mut params: std::collections::BTreeMap<String, String> = example
        .view
        .iter()
        .filter(|(_, value)| !value.is_null())
        .map(|(key, value)| {
            let value = match value {
                serde_json::Value::String(value) => value.clone(),
                serde_json::Value::Array(values) => values
                    .iter()
                    .map(|value| value.as_str().expect("example string list"))
                    .collect::<Vec<_>>()
                    .join(","),
                value => value.to_string(),
            };
            (key.clone(), value)
        })
        .collect();
    for (key, value) in form_urlencoded::parse(query.as_bytes()) {
        if key != "q" {
            params.insert(key.into_owned(), value.into_owned());
        }
    }
    form_urlencoded::Serializer::new(String::new())
        .extend_pairs(params)
        .finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_example_has_one_direct_path_and_preserves_its_search_values() {
        for example in examples() {
            let path = path(example);
            assert_eq!(for_path(&path).unwrap().id, example.id);
            assert_eq!(for_path(&format!("{path}/")).unwrap().id, example.id);
            let query = view_query(example, "");
            let raw = usnm_core::params::RawParams::parse(&query).unwrap();
            let expected = usnm_core::params::RawParams::parse(&example.aggregate).unwrap();
            for key in [
                "q", "from", "to", "bucket", "state", "lang", "lccn", "source", "series",
            ] {
                assert_eq!(raw.get(key), expected.get(key), "{path}: {key}");
            }
            assert_eq!(
                raw.get("mode").unwrap_or("phrase"),
                expected.get("mode").unwrap_or("phrase")
            );
            assert!(super::super::search_redirect(&path.parse().unwrap()).is_none());
        }
        for path in [
            "/examples/",
            "/examples/unknown",
            "/examples/whigs/extra",
            "/examples/%77higs",
        ] {
            assert!(for_path(path).is_none(), "{path}");
        }
        let example = for_path("/examples/whigs").unwrap();
        let query = view_query(example, "q=wrong&from=1840-01-01&tab=table");
        let params: std::collections::BTreeMap<_, _> =
            form_urlencoded::parse(query.as_bytes()).collect();
        assert_eq!(params["q"], "Whig OR Whigs");
        assert_eq!(params["from"], "1840-01-01");
        assert_eq!(params["to"], "1856-12-31");
        assert_eq!(params["tab"], "table");
    }
}
