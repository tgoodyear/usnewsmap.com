//! `robots.txt`, read and obeyed: the groups for this tool's user agent and for `*`, the
//! longest matching rule winning (an `Allow` wins a tie), and the crawl delay.

/// A parsed `robots.txt`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Robots {
    groups: Vec<Group>,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct Group {
    agents: Vec<String>,
    /// `(allow, pattern)`.
    rules: Vec<(bool, String)>,
    /// Seconds between requests, from `Crawl-delay` or `Request-rate`.
    delay: Option<f64>,
}

impl Robots {
    /// A site with no `robots.txt` (404 or 410) allows everything.
    pub fn allow_all() -> Self {
        Self::default()
    }

    pub fn parse(text: &str) -> Self {
        let mut groups: Vec<Group> = Vec::new();
        let mut last_was_agent = false;
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or_default().trim();
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let (key, value) = (key.trim().to_ascii_lowercase(), value.trim());
            match key.as_str() {
                "user-agent" => {
                    if !last_was_agent || groups.is_empty() {
                        groups.push(Group::default());
                    }
                    if let Some(g) = groups.last_mut() {
                        g.agents.push(value.to_ascii_lowercase());
                    }
                    last_was_agent = true;
                    continue;
                }
                "allow" | "disallow" => {
                    if let Some(g) = groups.last_mut() {
                        if !value.is_empty() {
                            g.rules.push((key == "allow", value.to_owned()));
                        }
                    }
                }
                "crawl-delay" => {
                    if let (Some(g), Ok(d)) = (groups.last_mut(), value.parse::<f64>()) {
                        g.delay = Some(g.delay.unwrap_or(0.0).max(d));
                    }
                }
                "request-rate" => {
                    // `1/3`: one request per three seconds.
                    let rate = value.split_whitespace().next().unwrap_or_default();
                    if let Some((n, s)) = rate.split_once('/') {
                        let s: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
                        if let (Some(g), Ok(n), Ok(s)) =
                            (groups.last_mut(), n.parse::<f64>(), s.parse::<f64>())
                        {
                            if n > 0.0 {
                                g.delay = Some(g.delay.unwrap_or(0.0).max(s / n));
                            }
                        }
                    }
                }
                _ => {}
            }
            last_was_agent = false;
        }
        Self { groups }
    }

    /// The groups naming `token` (a product token such as `usnewsmap-ocr-worker`), if any.
    fn named(&self, token: &str) -> Vec<&Group> {
        let token = token.to_ascii_lowercase();
        self.groups
            .iter()
            .filter(|g| {
                g.agents
                    .iter()
                    .any(|a| a != "*" && !a.is_empty() && token.starts_with(a.as_str()))
            })
            .collect()
    }

    fn star(&self) -> Vec<&Group> {
        self.groups
            .iter()
            .filter(|g| g.agents.iter().any(|a| a == "*"))
            .collect()
    }

    fn verdict(groups: &[&Group], path: &str) -> bool {
        let mut best: Option<(usize, bool)> = None;
        for (allow, pat) in groups.iter().flat_map(|g| g.rules.iter()) {
            if matches(pat, path) {
                let len = pat.len();
                best = match best {
                    Some((l, a)) if l > len || (l == len && a) => Some((l, a)),
                    _ => Some((len, *allow)),
                };
            }
        }
        best.is_none_or(|(_, allow)| allow)
    }

    /// Whether `path` (with its query) may be fetched by `token` and by `*`: both must allow
    /// it, so a rule for either refuses.
    pub fn allows(&self, token: &str, path: &str) -> bool {
        let named = self.named(token);
        let star = self.star();
        Self::verdict(&star, path) && (named.is_empty() || Self::verdict(&named, path))
    }

    /// The longest delay between requests any applicable group asks for, in seconds.
    pub fn delay_secs(&self, token: &str) -> f64 {
        self.named(token)
            .into_iter()
            .chain(self.star())
            .filter_map(|g| g.delay)
            .fold(0.0, f64::max)
    }
}

/// `robots.txt` pattern matching: a prefix, `*` for any run, `$` for the end.
fn matches(pattern: &str, path: &str) -> bool {
    fn go(p: &[u8], s: &[u8]) -> bool {
        match p.split_first() {
            None => true,
            Some((b'$', [])) => s.is_empty(),
            Some((b'*', rest)) => (0..=s.len()).any(|i| go(rest, &s[i..])),
            Some((c, rest)) => s.first() == Some(c) && go(rest, &s[1..]),
        }
    }
    go(pattern.as_bytes(), path.as_bytes())
}
