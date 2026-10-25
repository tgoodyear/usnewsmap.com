//! Where a Newspaper Navigator row's page is, and its crop on tile.loc.gov (#1102).
//!
//! Pure functions, no I/O. A page is named by the file it was scanned to,
//! `data/{lccn}/{reel}/{issue}/{frame}.jp2` in an NDNP batch, where `frame` is
//! the JP2's number on its reel. That is not the page's place in its issue
//! (`seq-n`): the dataset's `page_seq_num` is the frame, and turning a frame
//! into a `seq` needs the issue's file list from LoC (04 §4.11).
//!
//! The dataset's crop URLs point at the old `chroniclingamerica.loc.gov/iiif/2/`
//! server, which answers 308 to the same image on `tile.loc.gov`. The rewrite
//! here builds that target without asking, from the URL alone, and gives up on a
//! URL whose shape it cannot place rather than guess a tile id.

use chrono::NaiveDate;

/// The old IIIF server, as the dataset's `prediction_section_iiif_url` has it.
pub const OLD_IIIF: &str = "https://chroniclingamerica.loc.gov/iiif/2/";

/// The IIIF root on tile.loc.gov (the same one `usnm-ocr-worker` builds its
/// image URLs from).
pub const TILE_IIIF: &str = "https://tile.loc.gov/image-services/iiif/";

/// A frame is at least four digits (`0037`) and five or more occur
/// (`10283`); the upper end only keeps an issue directory from being read as
/// one when the real frame is missing.
const FRAME_DIGITS: std::ops::RangeInclusive<usize> = 4..=8;

/// The longest batch name LoC uses (`usnm-ocr-worker`'s `batch_dir`).
const MAX_BATCH_LEN: usize = 128;

/// One scan in a batch's directory tree. Segments keep the spelling the path
/// has, so the tile id built from them is the path's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scan {
    /// The title's LCCN as the path spells it (`sn85026051`; some paths have
    /// `SN85038614`). [`Scan::lccn_key`] is the lower-case form to join on.
    pub lccn: String,
    /// The reel directory: digits (`00296027029`), `print`, or two segments
    /// (`no/reel`), joined with `/`.
    pub reel: String,
    /// The issue directory, `YYYYMMDDEE` (date and edition).
    pub issue: String,
    pub date: NaiveDate,
    pub edition: u32,
    /// The JP2's name without `.jp2`, zero padded as in the path (`0037`).
    pub frame: String,
}

impl Scan {
    /// The LCCN in lower case, as the curated pages and the catalog spell it.
    pub fn lccn_key(&self) -> String {
        self.lccn.to_ascii_lowercase()
    }

    /// Whether two scans are the same one (the LCCN's case aside).
    fn same_as(&self, other: &Scan) -> bool {
        self.lccn.eq_ignore_ascii_case(&other.lccn)
            && self.reel == other.reel
            && self.issue == other.issue
            && self.frame == other.frame
    }
}

/// The scan a `page_url` names: its path ends `{lccn}/{reel}/{issue}/{frame}.jp2`,
/// whatever comes before. The dataset's own `lccn` column is not used: it is
/// the literal `no` on some rows.
pub fn scan_of_page_url(page_url: &str) -> Option<Scan> {
    let without_query = page_url.split(['?', '#']).next()?;
    let (_, path) = without_query.split_once("://")?;
    let segments: Vec<&str> = path.split('/').collect();
    scan_of(&segments, false)
}

/// A crop URL on the old server, taken apart: the batch, the scan, and the
/// `{region}/{size}/{rotation}/{quality}.jpg` tail, which is kept as it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OldCrop {
    /// The batch directory (`ohi_ingstad_ver01`).
    pub batch: String,
    pub scan: Scan,
    tail: String,
}

impl OldCrop {
    /// Parse `https://chroniclingamerica.loc.gov/iiif/2/{batch}%2Fdata%2F{lccn}%2F{reel}%2F{issue}%2F{frame}.jp2/{tail}`.
    /// `None` for any other shape, such as a URL whose image id has no frame
    /// (`…%2F{issue}.jp2`, which the dataset has on a few rows), a region in
    /// pixels, or a path with anything but `%2F` encoded in it.
    pub fn parse(old: &str) -> Option<Self> {
        let (id, tail) = old.strip_prefix(OLD_IIIF)?.split_once('/')?;
        if !is_iiif_tail(tail) {
            return None;
        }
        let id = id.replace("%2F", "/").replace("%2f", "/");
        if id.contains('%') {
            return None;
        }
        let segments: Vec<&str> = id.split('/').collect();
        let [batch, "data", rest @ ..] = segments.as_slice() else {
            return None;
        };
        if !is_batch(batch) {
            return None;
        }
        Some(Self {
            batch: (*batch).to_owned(),
            scan: scan_of(rest, true)?,
            tail: tail.to_owned(),
        })
    }

    /// The same crop on tile.loc.gov:
    /// `service:ndnp:{awardee}:batch_{batch}:data:{lccn}:{reel}:{issue}:{frame}/{tail}`.
    /// The awardee is the batch name's first `_` token, and slashes in the
    /// reel become colons, as `usnm-ocr-worker`'s `iiif_base` makes them. The
    /// region, size, rotation and quality stay as the old URL has them, which
    /// is what LoC's 308 answers with.
    pub fn tile_url(&self) -> String {
        let awardee = self.batch.split('_').next().unwrap_or_default();
        let scan = &self.scan;
        format!(
            "{TILE_IIIF}service:ndnp:{awardee}:batch_{}:data:{}:{}:{}:{}/{}",
            self.batch,
            scan.lccn,
            scan.reel.replace('/', ":"),
            scan.issue,
            scan.frame,
            self.tail
        )
    }

    /// Whether the crop is of the scan (and batch) a row's other columns name.
    pub fn is_of(&self, batch: Option<&str>, scan: Option<&Scan>) -> bool {
        batch.is_none_or(|b| b == self.batch) && scan.is_none_or(|s| self.scan.same_as(s))
    }
}

/// The crop URL on tile.loc.gov for one on the old server; `None` when the old
/// URL's shape is not one this places.
pub fn rewrite_crop_url(old: &str) -> Option<String> {
    OldCrop::parse(old).map(|c| c.tile_url())
}

/// `[lccn, reel…, issue, frame.jp2]` at the end of `segments` (all of it with
/// `anchored`, for an image id that starts at the LCCN).
fn scan_of(segments: &[&str], anchored: bool) -> Option<Scan> {
    let (file, rest) = segments.split_last()?;
    let frame = file.strip_suffix(".jp2")?;
    if !FRAME_DIGITS.contains(&frame.len()) || !frame.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (issue, rest) = rest.split_last()?;
    let (date, edition) = issue_of(issue)?;
    // The reel is one directory, or two (`no/reel`): the LCCN is the one
    // before it.
    for reel_len in 1..=2 {
        let Some(at) = rest.len().checked_sub(reel_len + 1) else {
            break;
        };
        if !is_lccn(rest[at]) || (anchored && at != 0) {
            continue;
        }
        let reel = &rest[at + 1..];
        if !reel.iter().all(|s| is_segment(s)) {
            return None;
        }
        return Some(Scan {
            lccn: rest[at].to_owned(),
            reel: reel.join("/"),
            issue: (*issue).to_owned(),
            date,
            edition,
            frame: frame.to_owned(),
        });
    }
    None
}

/// `YYYYMMDDEE`: ten digits, a real date and an edition from 1.
fn issue_of(issue: &str) -> Option<(NaiveDate, u32)> {
    if issue.len() != 10 || !issue.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n = |range: std::ops::Range<usize>| issue[range].parse::<u32>().ok();
    let date = NaiveDate::from_ymd_opt(i32::try_from(n(0..4)?).ok()?, n(4..6)?, n(6..8)?)?;
    let edition = n(8..10).filter(|e| *e >= 1)?;
    Some((date, edition))
}

/// Two letters and eight digits (`sn85026051`), in either case.
fn is_lccn(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[..2].iter().all(u8::is_ascii_alphabetic)
        && b[2..].iter().all(u8::is_ascii_digit)
}

/// A directory name that cannot change the path it is put into.
fn is_segment(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_BATCH_LEN
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

/// `{awardee}_{name}_ver{NN}`: at least an awardee and a name, so the awardee
/// token exists.
fn is_batch(s: &str) -> bool {
    is_segment(s) && s.split('_').count() >= 2 && s.split('_').all(|t| !t.is_empty())
}

/// `pct:{x},{y},{w},{h}/{size}/{rotation}/{quality}.jpg`, every part made of
/// the characters IIIF uses and nothing that would end the URL early.
fn is_iiif_tail(tail: &str) -> bool {
    let parts: Vec<&str> = tail.split('/').collect();
    let [region, size, rotation, quality] = parts.as_slice() else {
        return false;
    };
    let region_ok = region.strip_prefix("pct:").is_some_and(|numbers| {
        let numbers: Vec<&str> = numbers.split(',').collect();
        numbers.len() == 4
            && numbers.iter().all(|n| {
                !n.is_empty()
                    && n.bytes().filter(|b| *b == b'.').count() <= 1
                    && n.bytes().all(|b| b.is_ascii_digit() || b == b'.')
            })
    });
    let plain = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b',' | b'.' | b'!'))
    };
    region_ok && plain(size) && plain(rotation) && plain(quality) && quality.ends_with(".jpg")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pair LoC's 308 was checked on: the old URL and its target.
    const OLD: &str = "https://chroniclingamerica.loc.gov/iiif/2/ohi_ingstad_ver01%2Fdata%2Fsn85026051%2F00296027029%2F1850072701%2F0037.jp2/pct:29.91,62.29,13.14,10.85/pct:100/0/default.jpg";
    const NEW: &str = "https://tile.loc.gov/image-services/iiif/service:ndnp:ohi:batch_ohi_ingstad_ver01:data:sn85026051:00296027029:1850072701:0037/pct:29.91,62.29,13.14,10.85/pct:100/0/default.jpg";

    fn old(id: &str, tail: &str) -> String {
        format!("{OLD_IIIF}{id}/{tail}")
    }

    const TAIL: &str = "pct:1.5,2.5,10,20/pct:100/0/default.jpg";

    #[test]
    fn rewrites_the_pair_the_redirect_was_checked_on() {
        assert_eq!(rewrite_crop_url(OLD).as_deref(), Some(NEW));
        let crop = OldCrop::parse(OLD).unwrap();
        assert_eq!(crop.batch, "ohi_ingstad_ver01");
        assert_eq!(crop.scan.lccn, "sn85026051");
        assert_eq!(crop.scan.reel, "00296027029");
        assert_eq!(
            crop.scan.date,
            NaiveDate::from_ymd_opt(1850, 7, 27).unwrap()
        );
        assert_eq!((crop.scan.edition, crop.scan.frame.as_str()), (1, "0037"));
    }

    #[test]
    fn the_awardee_is_the_batch_names_first_token() {
        // Pairs the repo already uses (`usnm-ocr-worker`, `usnm-boinc`), the
        // one LoC redirected, and the shapes the dataset has.
        for (batch, awardee) in [
            ("ohi_ingstad_ver01", "ohi"),
            ("mdu_garlic_ver02", "mdu"),
            ("nbu_irons_ver02", "nbu"),
            ("dlc_abc_ver01", "dlc"),
            ("wa_bittern_ver01", "wa"),
            ("vi_darwin_ver01", "vi"),
            ("mdu_bode_ver03", "mdu"),
            // Mixed case is LoC's spelling and stays as it is.
            ("curiv_joshuaTree_ver01", "curiv"),
        ] {
            let url = rewrite_crop_url(&old(
                &format!("{batch}%2Fdata%2Fsn83045462%2F00280654021%2F1890050301%2F0001.jp2"),
                TAIL,
            ))
            .unwrap();
            assert_eq!(
                url,
                format!(
                    "{TILE_IIIF}service:ndnp:{awardee}:batch_{batch}:data:sn83045462:00280654021:1890050301:0001/{TAIL}"
                )
            );
        }
    }

    #[test]
    fn keeps_the_region_and_the_tail_as_they_are() {
        for tail in [
            "pct:29.91,62.29,13.14,10.85/pct:100/0/default.jpg",
            "pct:0,0,100,100/full/0/default.jpg",
            "pct:7.123456,8,9.9,0.01/pct:100/0/default.jpg",
        ] {
            let url = rewrite_crop_url(&old(
                "xx_test_ver01%2Fdata%2Fsn83045462%2F001%2F1890050301%2F0001.jp2",
                tail,
            ))
            .unwrap();
            assert!(url.ends_with(&format!("/{tail}")), "{url}");
        }
    }

    #[test]
    fn places_the_shapes_the_cartoons_config_has() {
        // A frame of five digits and the reel `print` (`mdu_bode_ver03`).
        let bode = OldCrop::parse(&old(
            "mdu_bode_ver03%2Fdata%2Fsn83045462%2Fprint%2F1905063001%2F10283.jp2",
            TAIL,
        ))
        .unwrap();
        assert_eq!(
            (bode.scan.reel.as_str(), bode.scan.frame.as_str()),
            ("print", "10283")
        );
        assert!(bode
            .tile_url()
            .contains(":batch_mdu_bode_ver03:data:sn83045462:print:1905063001:10283/"));

        // An upper-case LCCN (`vi_darwin_ver01`): the path's spelling goes into
        // the tile id, and the key to join on is lower case.
        let darwin = OldCrop::parse(&old(
            "vi_darwin_ver01%2Fdata%2FSN85038614%2F00296027029%2F1850072701%2F0037.jp2",
            TAIL,
        ))
        .unwrap();
        assert_eq!(darwin.scan.lccn, "SN85038614");
        assert_eq!(darwin.scan.lccn_key(), "sn85038614");
        assert!(darwin.tile_url().contains(":data:SN85038614:"));
    }

    #[test]
    fn will_not_guess_a_tile_id_for_a_url_it_cannot_place() {
        // `wa_bittern_ver01` and `wa_auklet_ver01`: the image id ends at the
        // issue, with no frame, and the reel is two directories.
        assert_eq!(
            rewrite_crop_url(&old(
                "wa_bittern_ver01%2Fdata%2Fsn84022793%2Fno%2Freel%2F1873100901.jp2",
                TAIL
            )),
            None
        );
        for bad in [
            // Not the old server.
            "https://example.org/iiif/2/ohi_ingstad_ver01%2Fdata%2Fsn85026051%2F00296027029%2F1850072701%2F0037.jp2/pct:1,2,3,4/pct:100/0/default.jpg",
            // Not `data`, an unencoded slash, another escape, a short frame,
            // a bad date or edition, a name that is no batch.
            "https://chroniclingamerica.loc.gov/iiif/2/ohi_ingstad_ver01%2Fdta%2Fsn85026051%2F00296027029%2F1850072701%2F0037.jp2/pct:1,2,3,4/pct:100/0/default.jpg",
            "https://chroniclingamerica.loc.gov/iiif/2/ohi_ingstad_ver01/data/sn85026051/00296027029/1850072701/0037.jp2/pct:1,2,3,4/pct:100/0/default.jpg",
            "https://chroniclingamerica.loc.gov/iiif/2/ohi_ingstad_ver01%2Fdata%2Fsn85026051%2F00296027029%2F1850072701%2F0037%2520.jp2/pct:1,2,3,4/pct:100/0/default.jpg",
            "https://chroniclingamerica.loc.gov/iiif/2/ohi_ingstad_ver01%2Fdata%2Fsn85026051%2F00296027029%2F1850072701%2F037.jp2/pct:1,2,3,4/pct:100/0/default.jpg",
            "https://chroniclingamerica.loc.gov/iiif/2/ohi_ingstad_ver01%2Fdata%2Fsn85026051%2F00296027029%2F1850133301%2F0037.jp2/pct:1,2,3,4/pct:100/0/default.jpg",
            "https://chroniclingamerica.loc.gov/iiif/2/ohi_ingstad_ver01%2Fdata%2Fsn85026051%2F00296027029%2F1850072700%2F0037.jp2/pct:1,2,3,4/pct:100/0/default.jpg",
            "https://chroniclingamerica.loc.gov/iiif/2/ohio%2Fdata%2Fsn85026051%2F00296027029%2F1850072701%2F0037.jp2/pct:1,2,3,4/pct:100/0/default.jpg",
            // Not an LCCN.
            "https://chroniclingamerica.loc.gov/iiif/2/ohi_ingstad_ver01%2Fdata%2Fxx%2F00296027029%2F1850072701%2F0037.jp2/pct:1,2,3,4/pct:100/0/default.jpg",
            // A region in pixels, a short region, a tail that could end the URL early.
            "https://chroniclingamerica.loc.gov/iiif/2/ohi_ingstad_ver01%2Fdata%2Fsn85026051%2F00296027029%2F1850072701%2F0037.jp2/10,20,30,40/pct:100/0/default.jpg",
            "https://chroniclingamerica.loc.gov/iiif/2/ohi_ingstad_ver01%2Fdata%2Fsn85026051%2F00296027029%2F1850072701%2F0037.jp2/pct:1,2,3/pct:100/0/default.jpg",
            "https://chroniclingamerica.loc.gov/iiif/2/ohi_ingstad_ver01%2Fdata%2Fsn85026051%2F00296027029%2F1850072701%2F0037.jp2/pct:1,2,3,4/pct:100/0/default.jpg?x=1",
            "https://chroniclingamerica.loc.gov/iiif/2/ohi_ingstad_ver01%2Fdata%2Fsn85026051%2F00296027029%2F1850072701%2F0037.jp2/pct:1,2,3,4/pct:100/0",
            "",
        ] {
            assert_eq!(rewrite_crop_url(bad), None, "{bad}");
        }
    }

    #[test]
    fn reads_the_scan_from_a_page_url() {
        let base = "https://chroniclingamerica.loc.gov/data/batches";
        // The standard shape.
        let s = scan_of_page_url(&format!(
            "{base}/ohi_ingstad_ver01/data/sn85026051/00296027029/1850072701/0037.jp2"
        ))
        .unwrap();
        assert_eq!(s.lccn, "sn85026051");
        assert_eq!(s.reel, "00296027029");
        assert_eq!(s.issue, "1850072701");
        assert_eq!(s.date, NaiveDate::from_ymd_opt(1850, 7, 27).unwrap());
        assert_eq!((s.edition, s.frame.as_str()), (1, "0037"));

        // `wa_bittern_ver01`: no reel number, two directories, and the frame is
        // in the page URL even though the crop URL lacks it.
        let s = scan_of_page_url(&format!(
            "{base}/wa_bittern_ver01/data/sn84022793/no/reel/1873100901/0368.jp2"
        ))
        .unwrap();
        assert_eq!(
            (s.lccn.as_str(), s.reel.as_str()),
            ("sn84022793", "no/reel")
        );
        assert_eq!((s.issue.as_str(), s.frame.as_str()), ("1873100901", "0368"));
        assert_eq!(s.date, NaiveDate::from_ymd_opt(1873, 10, 9).unwrap());

        // `mdu_bode_ver03`: reel `print`, five digits.
        let s = scan_of_page_url(&format!(
            "{base}/mdu_bode_ver03/data/sn83045462/print/1905063002/10283.jp2"
        ))
        .unwrap();
        assert_eq!(
            (s.reel.as_str(), s.frame.as_str(), s.edition),
            ("print", "10283", 2)
        );

        // `vi_darwin_ver01`: upper case.
        let s = scan_of_page_url(&format!(
            "{base}/vi_darwin_ver01/data/SN85038614/00296027029/1850072701/0037.jp2"
        ))
        .unwrap();
        assert_eq!(
            (s.lccn.as_str(), s.lccn_key().as_str()),
            ("SN85038614", "sn85038614")
        );

        // Whatever precedes the scan's directories, and a query string.
        assert!(scan_of_page_url(
            "https://example.test/sn85026051/00296027029/1850072701/0037.jp2?x=1"
        )
        .is_some());

        for bad in [
            "",
            "sn85026051/00296027029/1850072701/0037.jp2",
            "https://example.test/sn85026051/00296027029/1850072701/0037.tif",
            "https://example.test/sn85026051/00296027029/1850072701/37.jp2",
            "https://example.test/sn85026051/00296027029/1850072701.jp2",
            "https://example.test/sn85026051//1850072701/0037.jp2",
            "https://example.test/ab/00296027029/1850072701/0037.jp2",
            "https://example.test/sn85026051/a/b/c/1850072701/0037.jp2",
        ] {
            assert_eq!(scan_of_page_url(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_crop_is_of_the_scan_the_row_names() {
        let crop = OldCrop::parse(OLD).unwrap();
        let same = scan_of_page_url(
            "https://chroniclingamerica.loc.gov/data/batches/ohi_ingstad_ver01/data/SN85026051/00296027029/1850072701/0037.jp2",
        )
        .unwrap();
        assert!(crop.is_of(Some("ohi_ingstad_ver01"), Some(&same)));
        assert!(crop.is_of(None, None));
        // Another frame, another batch.
        let other = scan_of_page_url(
            "https://chroniclingamerica.loc.gov/data/batches/ohi_ingstad_ver01/data/sn85026051/00296027029/1850072701/0038.jp2",
        )
        .unwrap();
        assert!(!crop.is_of(Some("ohi_ingstad_ver01"), Some(&other)));
        assert!(!crop.is_of(Some("ohi_other_ver01"), Some(&same)));
    }
}
