//! Original LoC word confidence extracted from one ALTO stream.

use anyhow::bail;
use quick_xml::events::Event;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PageStats {
    pub doc_id: String,
    pub words: u32,
    pub words_with_wc: u32,
    pub wc_mean: Option<f64>,
    pub wc_median: Option<f64>,
    pub wc_below: Option<f64>,
    pub software: Option<String>,
}

pub fn page_stats<R: std::io::BufRead>(
    doc_id: String,
    xml: R,
    threshold: f64,
) -> anyhow::Result<PageStats> {
    let mut reader = quick_xml::Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut wcs: Vec<f64> = Vec::new();
    let mut words: u32 = 0;
    let mut software: Option<String> = None;
    let mut in_software = false;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e) | Event::Empty(e)) => {
                let name = e.local_name();
                if name.as_ref() == "String" {
                    words += 1;
                    for a in e.attributes().flatten() {
                        if a.key.as_ref() == "WC" {
                            if let Ok(v) = a.value.trim().parse::<f64>() {
                                if (0.0..=1.0).contains(&v) {
                                    wcs.push(v);
                                }
                            }
                        }
                    }
                } else if name.as_ref() == "softwareName" && software.is_none() {
                    in_software = true;
                }
            }
            Ok(Event::Text(t)) if in_software => {
                let s = t
                    .xml_content(quick_xml::XmlVersion::Implicit1_0)
                    .trim()
                    .to_owned();
                if !s.is_empty() {
                    software = Some(s.chars().take(64).collect());
                }
                in_software = false;
            }
            Ok(Event::End(_)) => in_software = false,
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => bail!("{doc_id}: unreadable ALTO: {e}"),
        }
        buf.clear();
    }
    let n = wcs.len();
    let round = |x: f64| (x * 1e6).round() / 1e6;
    let (mean, median, below) = if n == 0 {
        (None, None, None)
    } else {
        wcs.sort_by(f64::total_cmp);
        let mean = wcs.iter().sum::<f64>() / n as f64;
        let median = if n % 2 == 1 {
            wcs[n / 2]
        } else {
            (wcs[n / 2 - 1] + wcs[n / 2]) / 2.0
        };
        let below = wcs.iter().filter(|w| **w < threshold).count() as f64 / n as f64;
        (Some(round(mean)), Some(round(median)), Some(round(below)))
    };
    Ok(PageStats {
        doc_id,
        words,
        words_with_wc: u32::try_from(n).unwrap_or(u32::MAX),
        wc_mean: mean,
        wc_median: median,
        wc_below: below,
        software,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_and_invalid_confidence_does_not_count_as_zero() {
        let xml = br#"<alto><String WC="0.9"/><String WC="0.3"/><String WC="0.7"/>
            <String/><String WC="NaN"/><String WC="1.1"/></alto>"#;
        let s = page_stats("d".into(), xml.as_slice(), 0.5).unwrap();
        assert_eq!((s.words, s.words_with_wc), (6, 3));
        assert_eq!(
            (s.wc_mean, s.wc_median, s.wc_below),
            (Some(0.633333), Some(0.7), Some(0.333333))
        );
        let s = page_stats("d".into(), b"<alto><String/></alto>".as_slice(), 0.5).unwrap();
        assert_eq!((s.wc_mean, s.wc_median, s.wc_below), (None, None, None));
    }

    #[test]
    fn threshold_is_strict_and_malformed_xml_is_an_error() {
        let s = page_stats(
            "d".into(),
            br#"<alto><String WC="0.5"/><String WC="0.7"/></alto>"#.as_slice(),
            0.5,
        )
        .unwrap();
        assert_eq!(s.wc_median, Some(0.6));
        assert_eq!(s.wc_below, Some(0.0));
        assert!(page_stats("d".into(), b"<alto><String></alto>".as_slice(), 0.5).is_err());
    }
}
