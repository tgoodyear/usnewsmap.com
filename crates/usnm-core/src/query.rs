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

use crate::text::{fold, tokenize};

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
            let terms: Vec<Node> = tokenize(q).into_iter().map(exact).collect();
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
        Node::Term(t) if !t.prefix && t.fuzzy == 0 => Node::Term(Term { fuzzy, ..t }),
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
        Node::Term(_) | Node::Phrase { .. } => Ok(()),
    }
}

fn count_terms(node: &Node) -> usize {
    match node {
        Node::Term(_) => 1,
        Node::Phrase { terms, .. } => terms.len(),
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
                write!(f, "\"{}\"", terms.join(" "))?;
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
