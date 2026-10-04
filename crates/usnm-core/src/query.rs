//! The user query language (06 §6.4).
//!
//! ```text
//! query     := or_expr
//! or_expr   := and_expr ( "OR" and_expr )*
//! and_expr  := unary ( ["AND"] unary )*
//! unary     := ("-" | "NOT") primary | primary
//! primary   := PHRASE [ "~" INT ]
//!            | TERM [ "~" ("1" | "2") | "*" ]
//!            | "(" query ")"
//! ```
//!
//! Only the parsed AST ever reaches a search backend; raw user syntax is never
//! forwarded, which removes the legacy Solr injection risk structurally.

use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ja::{self, has_ja, tokenize, MAX_JA_RUN_CHARS};
use crate::text::fold;

pub const MAX_QUERY_CHARS: usize = 256;
pub const MAX_TERMS: usize = 12;
pub const MAX_OR_BRANCHES: usize = 4;
pub const MIN_PREFIX_CHARS: usize = 3;
pub const MAX_FUZZY: u8 = 2;
pub const MAX_SLOP: u8 = 20;
const MAX_DEPTH: usize = 8;

#[derive(Debug, Clone, Error, PartialEq, Eq, Serialize)]
#[error("{message}")]
pub struct QueryError {
    pub message: String,
    /// Character offset in the input where the problem was found.
    pub position: Option<usize>,
}

impl QueryError {
    fn at(message: impl Into<String>, position: usize) -> Self {
        Self {
            message: message.into(),
            position: Some(position),
        }
    }
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            position: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Term {
    /// Folded (lowercase, ASCII) token.
    pub text: String,
    /// Edit distance for OCR-tolerant matching (0 = exact).
    pub fuzzy: u8,
    pub prefix: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum Node {
    Term(Term),
    Phrase { terms: Vec<String>, slop: u8 },
    And(Vec<Node>),
    Or(Vec<Node>),
    Not(Box<Node>),
}

/// How a plain word list is combined (06 §6.3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Phrase,
    All,
    Any,
    Near,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Phrase => "phrase",
            Self::All => "all",
            Self::Any => "any",
            Self::Near => "near",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "phrase" => Some(Self::Phrase),
            "all" => Some(Self::All),
            "any" => Some(Self::Any),
            "near" => Some(Self::Near),
            _ => None,
        }
    }
}

/// Build the AST for a request: `q` in the query language, optionally combined
/// by `mode` when `q` is a plain word list, with `fuzzy` applied to exact terms.
pub fn build(q: &str, mode: Option<Mode>, near: u8, fuzzy: u8) -> Result<Node, QueryError> {
    let q = q.trim();
    if q.is_empty() {
        return Err(QueryError::new("query is empty"));
    }
    if q.chars().count() > MAX_QUERY_CHARS {
        return Err(QueryError::new(format!(
            "query is longer than {MAX_QUERY_CHARS} characters"
        )));
    }
    if fuzzy > MAX_FUZZY {
        return Err(QueryError::new(format!("fuzzy must be 0–{MAX_FUZZY}")));
    }
    let plain = is_plain(q);
    let node = match mode {
        None | Some(Mode::All) => parse(q)?,
        Some(m) if !plain => {
            return Err(QueryError::new(format!(
                "mode={} needs a plain list of words; use mode=all with query syntax",
                m.as_str()
            )))
        }
        Some(Mode::Phrase) => phrase(tokenize(q), 0)?,
        Some(Mode::Near) => {
            if near == 0 || near > MAX_SLOP {
                return Err(QueryError::new(format!("near must be 1–{MAX_SLOP}")));
            }
            phrase(tokenize(q), near)?
        }
        Some(Mode::Any) => {
            // A Japanese word is a phrase of characters, not one alternative per character.
            let mut terms: Vec<Node> = Vec::new();
            for w in q.split_whitespace() {
                if has_ja(w) {
                    terms.push(word(w, 0, false, 0)?);
                } else {
                    terms.extend(tokenize(w).into_iter().map(exact));
                }
            }
            match terms.len() {
                0 => return Err(QueryError::new("query has no searchable words")),
                1 => terms.into_iter().next().expect("one term"),
                _ => Node::Or(terms),
            }
        }
    };
    let node = apply_fuzzy(normalize(node), fuzzy);
    validate(&node)?;
    Ok(node)
}

/// Parse the query language.
pub fn parse(input: &str) -> Result<Node, QueryError> {
    let tokens = lex(input)?;
    let mut p = Parser {
        tokens,
        pos: 0,
        depth: 0,
        input_len: input.chars().count(),
    };
    let node = p.or_expr()?;
    if let Some(t) = p.peek() {
        return Err(QueryError::at("unexpected input", t.pos));
    }
    let node = normalize(node);
    validate(&node)?;
    Ok(node)
}

fn is_plain(q: &str) -> bool {
    !q.chars()
        .any(|c| matches!(c, '"' | '(' | ')' | '~' | '*' | ':'))
        && !q
            .split_whitespace()
            .any(|w| w == "AND" || w == "OR" || w == "NOT" || w.starts_with('-'))
}

fn exact(text: String) -> Node {
    Node::Term(Term {
        text,
        fuzzy: 0,
        prefix: false,
    })
}

fn phrase(terms: Vec<String>, slop: u8) -> Result<Node, QueryError> {
    match terms.len() {
        0 => Err(QueryError::new("query has no searchable words")),
        1 if slop == 0 => Ok(exact(terms.into_iter().next().expect("one term"))),
        _ => Ok(Node::Phrase { terms, slop }),
    }
}

// ---------------------------------------------------------------- lexer

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    LParen,
    RParen,
    Minus,
    And,
    Or,
    Not,
    Phrase {
        text: String,
        slop: u8,
    },
    Word {
        text: String,
        fuzzy: u8,
        prefix: bool,
    },
}

#[derive(Debug, Clone)]
struct Spanned {
    tok: Tok,
    pos: usize,
}

fn lex(input: &str) -> Result<Vec<Spanned>, QueryError> {
    let chars: Vec<char> = input.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let pos = i;
        match c {
            c if c.is_whitespace() => i += 1,
            '(' => {
                out.push(Spanned {
                    tok: Tok::LParen,
                    pos,
                });
                i += 1;
            }
            ')' => {
                out.push(Spanned {
                    tok: Tok::RParen,
                    pos,
                });
                i += 1;
            }
            '-' if i + 1 < chars.len() && !chars[i + 1].is_whitespace() => {
                out.push(Spanned {
                    tok: Tok::Minus,
                    pos,
                });
                i += 1;
            }
            '"' => {
                let start = i + 1;
                let end = chars[start..]
                    .iter()
                    .position(|&c| c == '"')
                    .map(|n| start + n)
                    .ok_or_else(|| QueryError::at("unterminated quote", pos))?;
                let text: String = chars[start..end].iter().collect();
                i = end + 1;
                let (slop, next) = read_tilde(&chars, i, MAX_SLOP, "phrase slop")?;
                i = next;
                out.push(Spanned {
                    tok: Tok::Phrase { text, slop },
                    pos,
                });
            }
            _ => {
                let start = i;
                while i < chars.len()
                    && !chars[i].is_whitespace()
                    && !matches!(chars[i], '(' | ')' | '"' | '~' | '*')
                {
                    if chars[i] == ':' {
                        return Err(QueryError::at("field syntax (':') is not supported", i));
                    }
                    i += 1;
                }
                let text: String = chars[start..i].iter().collect();
                if text.is_empty() {
                    let what = if chars[i] == '*' {
                        "wildcards must follow at least 3 letters"
                    } else {
                        "'~' must follow a word or phrase"
                    };
                    return Err(QueryError::at(what, i));
                }
                let tok = match text.as_str() {
                    "AND" => Tok::And,
                    "OR" => Tok::Or,
                    "NOT" => Tok::Not,
                    _ => {
                        let (fuzzy, prefix, next) = if i < chars.len() && chars[i] == '*' {
                            (0, true, i + 1)
                        } else {
                            let (f, next) = read_tilde(&chars, i, MAX_FUZZY, "fuzzy distance")?;
                            (f, false, next)
                        };
                        if next < chars.len() && matches!(chars[next], '*' | '~') {
                            return Err(QueryError::at(
                                "a term can be fuzzy or a prefix, not both",
                                next,
                            ));
                        }
                        i = next;
                        Tok::Word {
                            text,
                            fuzzy,
                            prefix,
                        }
                    }
                };
                out.push(Spanned { tok, pos: start });
            }
        }
    }
    Ok(out)
}

/// Read an optional `~N` at `i`. Returns (N, next index).
fn read_tilde(chars: &[char], i: usize, max: u8, what: &str) -> Result<(u8, usize), QueryError> {
    if i >= chars.len() || chars[i] != '~' {
        return Ok((0, i));
    }
    let start = i + 1;
    let mut end = start;
    while end < chars.len() && chars[end].is_ascii_digit() {
        end += 1;
    }
    let digits: String = chars[start..end].iter().collect();
    let n: u8 = digits
        .parse()
        .ok()
        .filter(|n| (1..=max).contains(n))
        .ok_or_else(|| QueryError::at(format!("{what} must be 1–{max}"), i))?;
    Ok((n, end))
}

// ---------------------------------------------------------------- parser

struct Parser {
    tokens: Vec<Spanned>,
    pos: usize,
    depth: usize,
    /// Input length in characters, for "expected more input" errors.
    input_len: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Spanned> {
        self.tokens.get(self.pos)
    }

    fn end_pos(&self) -> usize {
        self.input_len
    }

    fn or_expr(&mut self) -> Result<Node, QueryError> {
        let mut branches = vec![self.and_expr()?];
        while matches!(self.peek().map(|t| &t.tok), Some(Tok::Or)) {
            self.pos += 1;
            branches.push(self.and_expr()?);
        }
        Ok(if branches.len() == 1 {
            branches.pop().expect("one")
        } else {
            Node::Or(branches)
        })
    }

    fn and_expr(&mut self) -> Result<Node, QueryError> {
        let mut parts = vec![self.unary()?];
        loop {
            match self.peek().map(|t| &t.tok) {
                Some(Tok::And) => {
                    self.pos += 1;
                    parts.push(self.unary()?);
                }
                Some(Tok::Or | Tok::RParen) | None => break,
                Some(_) => parts.push(self.unary()?),
            }
        }
        Ok(if parts.len() == 1 {
            parts.pop().expect("one")
        } else {
            Node::And(parts)
        })
    }

    fn unary(&mut self) -> Result<Node, QueryError> {
        if matches!(self.peek().map(|t| &t.tok), Some(Tok::Minus | Tok::Not)) {
            self.pos += 1;
            return Ok(Node::Not(Box::new(self.primary()?)));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Node, QueryError> {
        let Some(t) = self.tokens.get(self.pos).cloned() else {
            return Err(QueryError::at(
                "expected a word, phrase or '('",
                self.end_pos(),
            ));
        };
        self.pos += 1;
        match t.tok {
            Tok::LParen => {
                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    return Err(QueryError::at("too many nested parentheses", t.pos));
                }
                let inner = self.or_expr()?;
                self.depth -= 1;
                match self.peek() {
                    Some(Spanned {
                        tok: Tok::RParen, ..
                    }) => {
                        self.pos += 1;
                        Ok(inner)
                    }
                    _ => Err(QueryError::at("missing ')'", t.pos)),
                }
            }
            Tok::Phrase { text, slop } => {
                phrase(tokenize(&text), slop).map_err(|e| QueryError::at(e.message, t.pos))
            }
            Tok::Word {
                text,
                fuzzy,
                prefix,
            } => word(&text, fuzzy, prefix, t.pos),
            Tok::RParen => Err(QueryError::at("unexpected ')'", t.pos)),
            Tok::And | Tok::Or | Tok::Not | Tok::Minus => Err(QueryError::at(
                "operator is missing a word before or after it",
                t.pos,
            )),
        }
    }
}

fn word(text: &str, fuzzy: u8, prefix: bool, pos: usize) -> Result<Node, QueryError> {
    if has_ja(text) {
        return ja_word(text, fuzzy, prefix, pos);
    }
    let tokens = tokenize(text);
    match tokens.len() {
        0 => Err(QueryError::at("word has no searchable characters", pos)),
        1 => {
            let text = tokens.into_iter().next().expect("one token");
            if prefix && text.chars().count() < MIN_PREFIX_CHARS {
                return Err(QueryError::at(
                    format!("prefix searches need at least {MIN_PREFIX_CHARS} letters"),
                    pos,
                ));
            }
            Ok(Node::Term(Term {
                text,
                fuzzy,
                prefix,
            }))
        }
        // "well-known" → the phrase "well known"
        _ if fuzzy == 0 && !prefix => Ok(Node::Phrase {
            terms: tokens,
            slop: 0,
        }),
        _ => Err(QueryError::at("fuzzy or prefix needs a single word", pos)),
    }
}

/// A word with Japanese in it: each run between punctuation (、。「」 …) is a
/// phrase of its characters, and the runs are ANDed. Prefix and fuzzy matching
/// work on whole words, which Japanese text doesn't mark, so they're refused.
fn ja_word(text: &str, fuzzy: u8, prefix: bool, pos: usize) -> Result<Node, QueryError> {
    if fuzzy > 0 || prefix {
        return Err(QueryError::at(
            "prefix (*) and fuzzy (~) searches aren't available for Japanese",
            pos,
        ));
    }
    let runs: Vec<Node> = text
        .split(|c: char| !c.is_alphanumeric())
        .map(tokenize)
        .filter(|t| !t.is_empty())
        .map(|t| phrase(t, 0))
        .collect::<Result<_, _>>()
        .map_err(|e| QueryError::at(e.message, pos))?;
    match runs.len() {
        0 => Err(QueryError::at("word has no searchable characters", pos)),
        1 => Ok(runs.into_iter().next().expect("one run")),
        _ => Ok(Node::And(runs)),
    }
}

/// Whether a term is Japanese (one Japanese character, as the tokenizer makes them).
fn is_ja_term(t: &str) -> bool {
    t.chars().count() == 1 && t.chars().all(ja::is_ja)
}

/// The lengths of the contiguous Japanese runs in a phrase's terms.
fn ja_runs(terms: &[String]) -> Vec<usize> {
    let mut runs = Vec::new();
    let mut len = 0;
    for t in terms {
        if is_ja_term(t) {
            len += 1;
        } else if len > 0 {
            runs.push(len);
            len = 0;
        }
    }
    if len > 0 {
        runs.push(len);
    }
    runs
}

/// Whether the query has a Japanese word that isn't excluded: it then searches
/// the Japanese pages (#139).
pub fn is_japanese(node: &Node) -> bool {
    match node {
        Node::Term(t) => is_ja_term(&t.text),
        Node::Phrase { terms, .. } => terms.iter().any(|t| is_ja_term(t)),
        Node::And(c) | Node::Or(c) => c.iter().any(is_japanese),
        Node::Not(_) => false,
    }
}

fn has_any_ja(node: &Node) -> bool {
    match node {
        Node::Term(t) => is_ja_term(&t.text),
        Node::Phrase { terms, .. } => terms.iter().any(|t| is_ja_term(t)),
        Node::And(c) | Node::Or(c) => c.iter().any(has_any_ja),
        Node::Not(n) => has_any_ja(n),
    }
}

// ---------------------------------------------------------------- normalize / validate

/// Flatten nested AND/OR, drop double negation, sort children for a stable canonical form.
pub fn normalize(node: Node) -> Node {
    match node {
        Node::And(children) => flatten(children, true),
        Node::Or(children) => flatten(children, false),
        Node::Not(inner) => match normalize(*inner) {
            Node::Not(x) => *x,
            x => Node::Not(Box::new(x)),
        },
        leaf => leaf,
    }
}

fn flatten(children: Vec<Node>, is_and: bool) -> Node {
    let mut out: Vec<Node> = Vec::new();
    for c in children.into_iter().map(normalize) {
        match (c, is_and) {
            (Node::And(inner), true) | (Node::Or(inner), false) => out.extend(inner),
            (c, _) => out.push(c),
        }
    }
    out.sort_by_key(|n| n.to_string());
    out.dedup();
    match out.len() {
        1 => out.pop().expect("one"),
        _ if is_and => Node::And(out),
        _ => Node::Or(out),
    }
}

fn apply_fuzzy(node: Node, fuzzy: u8) -> Node {
    if fuzzy == 0 {
        return node;
    }
    match node {
        Node::Term(t) if !t.prefix && t.fuzzy == 0 && !is_ja_term(&t.text) => {
            Node::Term(Term { fuzzy, ..t })
        }
        Node::And(c) => Node::And(c.into_iter().map(|n| apply_fuzzy(n, fuzzy)).collect()),
        Node::Or(c) => Node::Or(c.into_iter().map(|n| apply_fuzzy(n, fuzzy)).collect()),
        Node::Not(n) => Node::Not(Box::new(apply_fuzzy(*n, fuzzy))),
        other => other,
    }
}

fn validate(node: &Node) -> Result<(), QueryError> {
    let terms = count_terms(node);
    if terms > MAX_TERMS {
        return Err(QueryError::new(format!(
            "query has {terms} words; the limit is {MAX_TERMS}"
        )));
    }
    check(node)?;
    if has_any_ja(node) && !is_japanese(node) {
        return Err(QueryError::new(
            "a Japanese word can't only be excluded; search for one too",
        ));
    }
    if !has_positive(node) {
        return Err(QueryError::new(
            "query needs at least one word that is not excluded",
        ));
    }
    Ok(())
}

fn check(node: &Node) -> Result<(), QueryError> {
    match node {
        Node::Or(c) if c.len() > MAX_OR_BRANCHES => Err(QueryError::new(format!(
            "OR has {} alternatives; the limit is {MAX_OR_BRANCHES}",
            c.len()
        ))),
        Node::Or(c) => {
            if c.iter().any(|n| matches!(n, Node::Not(_))) {
                return Err(QueryError::new(
                    "an OR alternative can't be only an exclusion",
                ));
            }
            c.iter().try_for_each(check)
        }
        Node::And(c) => c.iter().try_for_each(check),
        Node::Not(n) => check(n),
        Node::Phrase { terms, .. } => {
            let longest = ja_runs(terms).into_iter().max().unwrap_or(0);
            if longest > MAX_JA_RUN_CHARS {
                return Err(QueryError::new(format!(
                    "a Japanese phrase has {longest} characters; the limit is {MAX_JA_RUN_CHARS}"
                )));
            }
            Ok(())
        }
        Node::Term(_) => Ok(()),
    }
}

fn count_terms(node: &Node) -> usize {
    match node {
        Node::Term(_) => 1,
        // The characters of a Japanese run are one word between them.
        Node::Phrase { terms, .. } => {
            let latin = terms.iter().filter(|t| !is_ja_term(t)).count();
            latin + ja_runs(terms).len()
        }
        Node::And(c) | Node::Or(c) => c.iter().map(count_terms).sum(),
        Node::Not(n) => count_terms(n),
    }
}

fn has_positive(node: &Node) -> bool {
    match node {
        Node::Term(_) | Node::Phrase { .. } => true,
        Node::Not(_) => false,
        Node::And(c) | Node::Or(c) => c.iter().any(has_positive),
    }
}

/// Words to highlight in the LoC viewer (positive terms only).
pub fn highlight_terms(node: &Node) -> Vec<String> {
    let mut out = Vec::new();
    collect_positive(node, &mut out);
    out.dedup();
    out
}

fn collect_positive(node: &Node, out: &mut Vec<String>) {
    match node {
        Node::Term(t) => out.push(t.text.clone()),
        Node::Phrase { terms, .. } => out.extend(terms.iter().cloned()),
        Node::And(c) | Node::Or(c) => c.iter().for_each(|n| collect_positive(n, out)),
        Node::Not(_) => {}
    }
}

impl fmt::Display for Node {
    /// Canonical query-language rendering; parsing it yields the same AST.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Node::Term(t) => {
                write!(f, "{}", t.text)?;
                if t.prefix {
                    write!(f, "*")
                } else if t.fuzzy > 0 {
                    write!(f, "~{}", t.fuzzy)
                } else {
                    Ok(())
                }
            }
            Node::Phrase { terms, slop } => {
                // Japanese characters run together, as they're written (and reparse the same).
                f.write_str("\"")?;
                for (i, t) in terms.iter().enumerate() {
                    if i > 0 && !(is_ja_term(&terms[i - 1]) && is_ja_term(t)) {
                        f.write_str(" ")?;
                    }
                    f.write_str(t)?;
                }
                f.write_str("\"")?;
                if *slop > 0 {
                    write!(f, "~{slop}")?;
                }
                Ok(())
            }
            Node::And(c) => join(f, c, " AND "),
            Node::Or(c) => join(f, c, " OR "),
            Node::Not(n) => match n.as_ref() {
                Node::And(_) | Node::Or(_) => write!(f, "-({n})"),
                _ => write!(f, "-{n}"),
            },
        }
    }
}

fn join(f: &mut fmt::Formatter<'_>, children: &[Node], sep: &str) -> fmt::Result {
    for (i, c) in children.iter().enumerate() {
        if i > 0 {
            f.write_str(sep)?;
        }
        match c {
            Node::And(_) | Node::Or(_) => write!(f, "({c})")?,
            _ => write!(f, "{c}")?,
        }
    }
    Ok(())
}

/// Fold a user string the same way terms are folded (for display/highlighting).
pub fn fold_text(s: &str) -> String {
    fold(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(t: &str) -> Node {
        exact(t.to_owned())
    }

    fn ja(s: &str) -> Node {
        Node::Phrase {
            terms: s.chars().map(|c| c.to_string()).collect(),
            slop: 0,
        }
    }

    #[test]
    fn japanese_words_are_character_phrases() {
        let n = parse("真珠湾").unwrap();
        assert_eq!(n, ja("真珠湾"));
        assert_eq!(n.to_string(), "\"真珠湾\"");
        assert_eq!(parse(&n.to_string()).unwrap(), n);
        assert!(is_japanese(&n));
        // Old forms fold, so both spellings are one query (and one cache key).
        assert_eq!(parse("戰爭").unwrap(), parse("戦争").unwrap());
        // A single character is a term.
        assert_eq!(parse("年").unwrap(), term("年"));
    }

    #[test]
    fn japanese_punctuation_splits_runs() {
        assert_eq!(
            parse("東京、大阪").unwrap(),
            Node::And(vec![ja("大阪"), ja("東京")])
        );
        assert_eq!(parse("「日本」").unwrap(), ja("日本"));
    }

    #[test]
    fn mixed_japanese_and_latin() {
        let n = parse("gold 東京").unwrap();
        assert_eq!(n, Node::And(vec![ja("東京"), term("gold")]));
        assert!(is_japanese(&n));
        assert_eq!(
            parse("Rocky新報").unwrap(),
            Node::Phrase {
                terms: vec!["rocky".into(), "新".into(), "報".into()],
                slop: 0
            }
        );
        assert!(!is_japanese(&parse("gold silver").unwrap()));
    }

    #[test]
    fn japanese_in_each_mode() {
        let any = build("東京 大阪 gold", Some(Mode::Any), 0, 0).unwrap();
        assert_eq!(any, Node::Or(vec![ja("大阪"), ja("東京"), term("gold")]));
        let near = build("真珠湾 攻撃", Some(Mode::Near), 3, 0).unwrap();
        assert_eq!(
            near,
            Node::Phrase {
                terms: "真珠湾攻撃".chars().map(String::from).collect(),
                slop: 3
            }
        );
        let phrase = build("真珠湾 攻撃", Some(Mode::Phrase), 0, 0).unwrap();
        assert_eq!(phrase, ja("真珠湾攻撃"));
        // fuzzy applies to Latin words only.
        let fz = build("gold 東京", Some(Mode::All), 0, 1).unwrap();
        assert_eq!(
            fz,
            Node::And(vec![
                ja("東京"),
                Node::Term(Term {
                    text: "gold".into(),
                    fuzzy: 1,
                    prefix: false
                })
            ])
        );
    }

    #[test]
    fn japanese_limits() {
        // A run is one word for the word limit...
        let long = "日本".repeat(8);
        assert!(parse(&long).is_ok());
        // ...up to MAX_JA_RUN_CHARS characters.
        assert!(parse(&"日".repeat(MAX_JA_RUN_CHARS + 1))
            .unwrap_err()
            .message
            .contains("Japanese phrase"));
        // The limit is per run: two 20-character runs either side of a Latin word are fine...
        let two_runs = format!("\"{} gold {}\"", "日".repeat(20), "月".repeat(20));
        assert!(parse(&two_runs).is_ok());
        // ...and each run is a term: 日 gold 月 is three.
        assert_eq!(count_terms(&parse("\"日 gold 月\"").unwrap()), 3);
        assert!(parse("東京*")
            .unwrap_err()
            .message
            .contains("aren't available for Japanese"));
        assert!(parse("東京~1")
            .unwrap_err()
            .message
            .contains("aren't available for Japanese"));
        assert!(parse("gold -東京")
            .unwrap_err()
            .message
            .contains("can't only be excluded"));
        assert!(parse("東京 -大阪").is_ok());
    }

    #[test]
    fn parses_phrases_terms_and_operators() {
        let n = parse(r#""Cross of Gold" silver -bryan"#).unwrap();
        assert_eq!(n.to_string(), r#""cross of gold" AND -bryan AND silver"#);
        let n = parse("scalawag OR carpetbagger").unwrap();
        assert_eq!(n, Node::Or(vec![term("carpetbagger"), term("scalawag")]));
        let n = parse(r#"("yellow fever" OR "yellow jack") AND memphis"#).unwrap();
        assert_eq!(
            n.to_string(),
            r#"("yellow fever" OR "yellow jack") AND memphis"#
        );
    }

    #[test]
    fn canonical_form_round_trips_and_is_order_independent() {
        let a = parse("silver gold").unwrap();
        let b = parse("gold AND silver").unwrap();
        assert_eq!(a, b);
        let again = parse(&a.to_string()).unwrap();
        assert_eq!(again, a);
        let n = parse(r#"NOT (a1 OR b1) keep "x y"~3 infl* colr~1"#).unwrap();
        assert_eq!(parse(&n.to_string()).unwrap(), n);
    }

    #[test]
    fn supports_slop_fuzzy_and_prefix() {
        assert_eq!(
            parse(r#""gold silver"~5"#).unwrap(),
            Node::Phrase {
                terms: vec!["gold".into(), "silver".into()],
                slop: 5
            }
        );
        assert_eq!(
            parse("miscegenaton~2").unwrap(),
            Node::Term(Term {
                text: "miscegenaton".into(),
                fuzzy: 2,
                prefix: false
            })
        );
        assert_eq!(
            parse("influ*").unwrap(),
            Node::Term(Term {
                text: "influ".into(),
                fuzzy: 0,
                prefix: true
            })
        );
    }

    #[test]
    fn hyphenated_word_becomes_phrase() {
        assert_eq!(
            parse("well-known").unwrap(),
            Node::Phrase {
                terms: vec!["well".into(), "known".into()],
                slop: 0
            }
        );
    }

    #[test]
    fn rejects_unsafe_or_invalid_queries() {
        let err = |q: &str| parse(q).unwrap_err();
        assert_eq!(err("text:gold").position, Some(4));
        assert!(err("*gold").message.contains("wildcard"));
        assert!(err("go*").message.contains("prefix"));
        assert!(err("gold~3").message.contains("fuzzy"));
        assert!(err(r#""a b"~21"#).message.contains("slop"));
        assert!(err(r#""cross of gold"#).message.contains("unterminated"));
        assert!(err("(gold").message.contains("')'"));
        assert!(err("-gold").message.contains("not excluded"));
        assert!(err("a1 OR b1 OR c1 OR d1 OR e1")
            .message
            .contains("OR has 5"));
        assert!(err("gold OR -silver").message.contains("only an exclusion"));
        let trailing = err("gold AND");
        assert!(trailing.message.contains("expected"));
        assert_eq!(trailing.position, Some(8));
        assert!(err("a1 b1 c1 d1 e1 f1 g1 h1 i1 j1 k1 l1 m1")
            .message
            .contains("limit is 12"));
        assert!(err("gold~1*").message.contains("not both"));
    }

    #[test]
    fn modes_combine_plain_word_lists() {
        assert_eq!(
            build("Cross of Gold", Some(Mode::Phrase), 0, 0)
                .unwrap()
                .to_string(),
            r#""cross of gold""#
        );
        assert_eq!(
            build("gold silver", Some(Mode::Any), 0, 0)
                .unwrap()
                .to_string(),
            "gold OR silver"
        );
        assert_eq!(
            build("gold silver", Some(Mode::Near), 5, 0)
                .unwrap()
                .to_string(),
            r#""gold silver"~5"#
        );
        assert_eq!(
            build("gold silver", None, 0, 1).unwrap().to_string(),
            "gold~1 AND silver~1"
        );
        assert!(build(r#""a b" c"#, Some(Mode::Any), 0, 0).is_err());
        assert!(build("gold silver", Some(Mode::Near), 0, 0).is_err());
        assert!(build("   ", None, 0, 0).is_err());
    }

    #[test]
    fn highlight_terms_skip_exclusions() {
        let n = parse(r#""cross of gold" -bryan"#).unwrap();
        assert_eq!(highlight_terms(&n), vec!["cross", "of", "gold"]);
    }
}
