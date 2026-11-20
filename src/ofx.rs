//! Reading OFX files (SPEC 17.2 to 17.5).
//!
//! Hand-rolled, because the spec reads OFX more leniently than the OFX
//! crates do. `ofx-rs` and `ofx-kit` apply OFX's own validation, which
//! SPEC 17.4 and 17.5 contradict: they require `FITID` and `TRNTYPE`, accept
//! only the `TRNTYPE`s OFX lists, and turn OFX 1.x into XML first, a step a
//! raw `&` (as in `CITY POWER & LIGHT`) breaks. `sgmlish`, unmaintained since
//! 2021, rejects `AT&T` as an unknown entity, where SPEC 17.3 keeps it as it
//! is. Decoding uses `encoding_rs`, so only the reading of headers, tags and
//! values is written here.
//!
//! A file is read in three steps: [`decode`] turns its bytes into text
//! (SPEC 17.2), [`parse`] turns the text into a tree of elements (SPEC 17.3),
//! and [`statements`] reads the statements out of the tree (SPEC 17.4, 17.5).

use encoding_rs::{Encoding, UTF_8, WINDOWS_1252};
use serde::{Deserialize, Serialize};

use crate::calendar;
use crate::fields;

/// What kind of account a statement is for (SPEC 17.4, 17.6).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatementType {
    /// A `STMTRS`.
    Bank,
    /// A `CCSTMTRS`.
    CreditCard,
}

/// A statement as the file has it (SPEC 17.4). Serializes as SPEC 17.6's
/// preview shows it, without the transactions.
#[derive(Debug, Serialize)]
pub struct Statement {
    #[serde(rename = "type")]
    pub kind: StatementType,
    pub bank_id: Option<String>,
    pub account_number: String,
    pub account_type: Option<String>,
    pub currency_code: Option<String>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub ledger_balance: Option<i64>,
    pub ledger_balance_date: Option<String>,
    #[serde(skip)]
    pub transactions: Vec<BankTransaction>,
}

/// A `STMTTRN` as the file has it (SPEC 17.4). Serializes as SPEC 17.6's
/// preview shows it, without its import id.
#[derive(Debug, Serialize)]
pub struct BankTransaction {
    #[serde(skip)]
    pub fitid: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub date: String,
    pub amount: i64,
    pub name: Option<String>,
    pub memo: Option<String>,
}

/// Reads the statements in a file, or says why it isn't OFX.
pub fn read(file: &[u8]) -> Result<Vec<Statement>, String> {
    statements(&parse(&decode(file)?)?)
}

// ---------------------------------------------------------------------------
// Headers and encodings (SPEC 17.2)

const BOM: &[u8] = b"\xEF\xBB\xBF";

/// The file's text, from its first `<`, in the encoding its headers name.
fn decode(file: &[u8]) -> Result<String, String> {
    let file = file.strip_prefix(BOM).unwrap_or(file);
    let start = file
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(file.len());
    let file = &file[start..];
    let (encoding, text) = if file.starts_with(b"<?xml") || file.starts_with(b"<?OFX") {
        (xml_encoding(file)?, file)
    } else {
        let body = file.iter().position(|&b| b == b'<').unwrap_or(file.len());
        let (headers, text) = file.split_at(body);
        (sgml_encoding(headers)?, text)
    };
    Ok(encoding.decode_without_bom_handling(text).0.into_owned())
}

/// The encoding of an OFX 1.x file from its header lines: UTF-8 when one is
/// `ENCODING:UTF-8`, and Windows-1252 otherwise.
fn sgml_encoding(headers: &[u8]) -> Result<&'static Encoding, String> {
    let mut lines = headers.split(|&b| b == b'\n').map(|line| {
        let (key, value) = line.split_at(line.iter().position(|&b| b == b':')?);
        Some((key.trim_ascii(), value[1..].trim_ascii()))
    });
    if lines.next() != Some(Some((&b"OFXHEADER"[..], &b"100"[..]))) {
        return Err("the file is neither OFX 1.x nor OFX 2.x".into());
    }
    let utf8 = lines.flatten().any(|(key, value)| {
        key.eq_ignore_ascii_case(b"ENCODING") && value.eq_ignore_ascii_case(b"UTF-8")
    });
    Ok(if utf8 { UTF_8 } else { WINDOWS_1252 })
}

/// The encoding an OFX 2.x file's XML declaration names, or UTF-8 without
/// one. Only UTF-8 and Windows-1252 are allowed, by any of their WHATWG
/// labels.
fn xml_encoding(file: &[u8]) -> Result<&'static Encoding, String> {
    let Some(declaration) = file
        .strip_prefix(b"<?xml")
        .and_then(|rest| Some(&rest[..find(rest, b"?>")?]))
    else {
        return Ok(UTF_8);
    };
    let Some(label) = pseudo_attribute(declaration, b"encoding") else {
        return Ok(UTF_8);
    };
    match Encoding::for_label_no_replacement(label) {
        Some(e) if e == UTF_8 || e == WINDOWS_1252 => Ok(e),
        _ => Err(format!(
            "the file's encoding, {}, isn't UTF-8 or Windows-1252",
            String::from_utf8_lossy(label)
        )),
    }
}

/// The value of `name` among an XML declaration's `name="value"` pairs.
fn pseudo_attribute<'a>(mut declaration: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    loop {
        declaration = declaration.trim_ascii_start();
        let eq = declaration.iter().position(|&b| b == b'=')?;
        let key = declaration[..eq].trim_ascii();
        let rest = declaration[eq + 1..].trim_ascii_start();
        let quote = *rest.first().filter(|q| matches!(q, b'"' | b'\''))?;
        let end = rest[1..].iter().position(|&b| b == quote)? + 1;
        if key == name {
            return Some(&rest[1..end]);
        }
        declaration = &rest[end + 1..];
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

// ---------------------------------------------------------------------------
// Elements (SPEC 17.3)

/// The elements of a file, in the order their start tags appear, the first
/// being the root, which holds the top-level elements. Elements refer to the
/// ones they hold by index, so that however deeply a file nests them, nothing
/// that reads, searches or drops the tree recurses.
struct Tree {
    elements: Vec<Element>,
}

/// An element: its name in uppercase, its value, and the indexes of the
/// elements it holds, in order.
#[derive(Default)]
struct Element {
    name: String,
    /// Absent if empty.
    value: Option<String>,
    children: Vec<usize>,
}

impl Tree {
    /// The root, which tests read from; statements are found among all
    /// elements, wherever they are.
    #[cfg(test)]
    fn root(&self) -> Node<'_> {
        self.node(0)
    }

    fn node(&self, index: usize) -> Node<'_> {
        Node { tree: self, index }
    }

    /// Every element, in the order their start tags appear.
    fn nodes(&self) -> impl Iterator<Item = Node<'_>> {
        (0..self.elements.len()).map(|i| self.node(i))
    }
}

/// An element of a tree, to read it and the elements it holds.
#[derive(Clone, Copy)]
struct Node<'a> {
    tree: &'a Tree,
    index: usize,
}

impl<'a> Node<'a> {
    fn element(self) -> &'a Element {
        &self.tree.elements[self.index]
    }

    fn name(self) -> &'a str {
        &self.element().name
    }

    fn value(self) -> Option<&'a str> {
        self.element().value.as_deref()
    }

    fn holds_elements(self) -> bool {
        !self.element().children.is_empty()
    }

    fn children(self) -> impl Iterator<Item = Node<'a>> {
        self.element().children.iter().map(|&i| self.tree.node(i))
    }

    fn children_named(self, name: &'a str) -> impl Iterator<Item = Node<'a>> {
        self.children().filter(move |c| c.name() == name)
    }

    /// Of the elements named `name` it holds directly, the first that holds
    /// other elements: SPEC 17.3's first that isn't absent, for an element
    /// whose elements SPEC 17.4 reads.
    fn child(self, name: &'a str) -> Option<Node<'a>> {
        self.children_named(name).find(|c| c.holds_elements())
    }

    /// Of the values of the elements named `name` it holds directly, the
    /// first that isn't absent (SPEC 17.3).
    fn value_of(self, name: &'a str) -> Option<&'a str> {
        self.children_named(name).find_map(Node::value)
    }
}

/// What [`parse`] keeps while it reads the text.
///
/// An element with no end tag holds only the text up to its first tag; what
/// comes after that, text and elements, belongs to its parent (SPEC 17.3).
/// Whether an element has an end tag is only known once its parent closes,
/// so the elements and text read inside the open elements are kept in two
/// lists shared by all of them, each open element's being the end of each
/// list from where it starts. Closing an element without an end tag then
/// leaves what came after its first tag in place, for its parent, without
/// moving it.
struct Parser<'a> {
    elements: Vec<Element>,
    /// The open elements, the root first.
    open: Vec<Open>,
    /// The indexes of the elements started inside the open elements, in
    /// order, that no closed element holds yet.
    children: Vec<usize>,
    /// The text read inside the open elements, in pieces: `None` once a
    /// value holds it.
    text: Vec<Option<&'a str>>,
}

/// An open element, and where what it holds starts in [`Parser`]'s lists.
struct Open {
    index: usize,
    children: usize,
    text: usize,
    /// Where the text up to its first tag ends, once a tag has come.
    head: Option<usize>,
}

impl<'a> Parser<'a> {
    fn new() -> Self {
        let root = Open {
            index: 0,
            children: 0,
            text: 0,
            head: None,
        };
        Parser {
            elements: vec![Element::default()],
            open: vec![root],
            children: Vec::new(),
            text: Vec::new(),
        }
    }

    fn text(&mut self, text: &'a str) {
        self.text.push(Some(text));
    }

    fn start(&mut self, name: &str) {
        self.end_head();
        let index = self.elements.len();
        self.elements.push(Element {
            name: name.to_ascii_uppercase(),
            ..Element::default()
        });
        self.children.push(index);
        self.open.push(Open {
            index,
            children: self.children.len(),
            text: self.text.len(),
            head: None,
        });
    }

    fn end(&mut self, name: &str) -> Result<(), String> {
        self.end_head();
        let depth = self
            .open
            .iter()
            .rposition(|o| self.elements[o.index].name.eq_ignore_ascii_case(name))
            .filter(|&depth| depth > 0)
            .ok_or_else(|| format!("the end tag </{name}> closes no open element"))?;
        while self.open.len() > depth + 1 {
            self.close_unended();
        }
        self.close();
        Ok(())
    }

    /// Closes what is still open at the end of the text, and returns the tree.
    fn finish(mut self) -> Tree {
        while self.open.len() > 1 {
            self.close_unended();
        }
        self.close();
        Tree {
            elements: self.elements,
        }
    }

    /// A tag ends the text the innermost open element holds if it turns out
    /// to have no end tag.
    fn end_head(&mut self) {
        let len = self.text.len();
        let top = self.open.last_mut().expect("the root stays open");
        top.head.get_or_insert(len);
    }

    /// Closes the innermost open element, which has no end tag of its own:
    /// it holds only the text up to its first tag.
    fn close_unended(&mut self) {
        let open = self.open.pop().expect("the caller leaves the root");
        let head = open.head.unwrap_or(self.text.len());
        self.elements[open.index].value = take_value(&mut self.text[open.text..head]);
    }

    /// Closes the innermost open element, by its end tag: it holds all that
    /// no element inside it holds.
    fn close(&mut self) {
        let open = self.open.pop().expect("the caller closes an open element");
        let element = &mut self.elements[open.index];
        element.value = take_value(&mut self.text[open.text..]);
        element.children = self.children.drain(open.children..).collect();
        self.text.truncate(open.text);
    }
}

/// The value of the pieces of text no value holds yet, which it takes.
fn take_value(pieces: &mut [Option<&str>]) -> Option<String> {
    value(
        &pieces
            .iter_mut()
            .filter_map(Option::take)
            .collect::<String>(),
    )
}

/// A piece of markup or text.
enum Token<'a> {
    Start(&'a str),
    End(&'a str),
    Text(&'a str),
}

/// Splits text into tags and text, skipping comments and processing
/// instructions wherever they are. Other markup starting `<!` is skipped up
/// to its `>`.
fn tokens(mut text: &str) -> impl Iterator<Item = Token<'_>> {
    std::iter::from_fn(move || {
        loop {
            if text.is_empty() {
                return None;
            }
            let Some(rest) = text.strip_prefix('<') else {
                let end = text.find('<').unwrap_or(text.len());
                let (piece, rest) = text.split_at(end);
                text = rest;
                return Some(Token::Text(piece));
            };
            let (skip_to, closer) = if rest.starts_with("!--") {
                (3, "-->")
            } else if rest.starts_with('?') {
                (1, "?>")
            } else {
                (0, ">")
            };
            let Some(end) = rest[skip_to..].find(closer).map(|i| i + skip_to) else {
                text = "";
                return None;
            };
            let tag = &rest[..end];
            text = &rest[end + closer.len()..];
            if skip_to > 0 || tag.starts_with('!') {
                continue;
            }
            return Some(match tag.strip_prefix('/') {
                Some(end) => Token::End(end.trim()),
                // What follows the name, such as attributes, is ignored.
                None => Token::Start(
                    tag.split(|c: char| c.is_whitespace() || c == '/')
                        .next()
                        .unwrap_or_default(),
                ),
            });
        }
    })
}

/// Reads the text into the elements it holds.
fn parse(text: &str) -> Result<Tree, String> {
    let mut parser = Parser::new();
    for token in tokens(text) {
        match token {
            Token::Text(text) => parser.text(text),
            Token::Start(name) => parser.start(name),
            Token::End(name) => parser.end(name)?,
        }
    }
    Ok(parser.finish())
}

/// The value of an element's text: references replaced, then white space
/// trimmed; `None` if nothing is left.
fn value(text: &str) -> Option<String> {
    let value = replace_references(text);
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// Replaces the named and numeric character references of SPEC 17.3; any
/// other `&` stays as it is.
fn replace_references(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp + 1..];
        // A reference's name is letters, digits and `#`, so the search for
        // its `;` stops at anything else, which keeps this linear.
        let len = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '#'))
            .unwrap_or(rest.len());
        let reference = rest[len..]
            .starts_with(';')
            .then(|| reference(&rest[..len]))
            .flatten();
        if let Some(c) = reference {
            out.push(c);
            rest = &rest[len + 1..];
        } else {
            out.push('&');
        }
    }
    out.push_str(rest);
    out
}

/// The character a reference's name, between `&` and `;`, stands for.
fn reference(name: &str) -> Option<char> {
    let number = |digits: &str, radix| {
        let valid = !digits.is_empty() && digits.chars().all(|c| c.is_digit(radix));
        char::from_u32(u32::from_str_radix(digits, radix).ok().filter(|_| valid)?)
    };
    match name {
        "lt" => Some('<'),
        "gt" => Some('>'),
        "amp" => Some('&'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some('\u{A0}'),
        _ => {
            let digits = name.strip_prefix('#')?;
            match digits.strip_prefix(['x', 'X']) {
                Some(hex) => number(hex, 16),
                None => number(digits, 10),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Statements (SPEC 17.4, 17.5)

/// The statements in the tree, wherever they are, even inside other
/// statements, in the order their start tags appear (SPEC 17.4).
fn statements(tree: &Tree) -> Result<Vec<Statement>, String> {
    let found: Vec<_> = tree
        .nodes()
        .filter(|n| matches!(n.name(), "STMTRS" | "CCSTMTRS"))
        .collect();
    if found.is_empty() {
        return Err("the file holds no statements".into());
    }
    found.into_iter().map(statement).collect()
}

fn statement(s: Node) -> Result<Statement, String> {
    let (kind, account_tag) = if s.name() == "STMTRS" {
        (StatementType::Bank, "BANKACCTFROM")
    } else {
        (StatementType::CreditCard, "CCACCTFROM")
    };
    let account = s.child(account_tag);
    let account_value = |name| account.and_then(|a| a.value_of(name)).map(str::to_owned);
    let bank_value = |name| account_value(name).filter(|_| kind == StatementType::Bank);
    let list = s.child("BANKTRANLIST");
    let list_date = |name| list.and_then(|l| l.value_of(name)).map(date).transpose();
    let ledger = s.child("LEDGERBAL");
    let ledger_value = |name| ledger.and_then(|l| l.value_of(name));
    Ok(Statement {
        kind,
        bank_id: bank_value("BANKID"),
        account_number: account_value("ACCTID").ok_or_else(|| required("ACCTID"))?,
        account_type: bank_value("ACCTTYPE"),
        currency_code: s.value_of("CURDEF").map(str::to_owned),
        start_date: list_date("DTSTART")?,
        end_date: list_date("DTEND")?,
        ledger_balance: ledger_value("BALAMT").map(amount).transpose()?,
        ledger_balance_date: ledger_value("DTASOF").map(date).transpose()?,
        transactions: list
            .map(|l| l.children_named("STMTTRN").map(transaction).collect())
            .transpose()?
            .unwrap_or_default(),
    })
}

fn transaction(t: Node) -> Result<BankTransaction, String> {
    let text = |name| t.value_of(name).map(str::to_owned);
    Ok(BankTransaction {
        fitid: text("FITID"),
        kind: text("TRNTYPE"),
        date: date(t.value_of("DTPOSTED").ok_or_else(|| required("DTPOSTED"))?)?,
        amount: amount(t.value_of("TRNAMT").ok_or_else(|| required("TRNAMT"))?)?,
        name: text("NAME").or_else(|| {
            t.child("PAYEE")
                .and_then(|p| p.value_of("NAME"))
                .map(str::to_owned)
        }),
        memo: text("MEMO"),
    })
}

fn required(name: &str) -> String {
    format!("a statement lacks {name}")
}

/// A date: eight digits `YYYYMMDD` naming a date in SPEC 3.8's range, and
/// anything after them, such as a time or time zone, which is ignored.
fn date(value: &str) -> Result<String, String> {
    let error = || format!("{value} isn't a date YYYYMMDD");
    // Checking the digits first makes the slicing below land on char boundaries.
    let digits = value
        .get(..8)
        .filter(|d| d.bytes().all(|b| b.is_ascii_digit()))
        .ok_or_else(error)?;
    let date = format!("{}-{}-{}", &digits[..4], &digits[4..6], &digits[6..]);
    if calendar::is_date(&date) {
        Ok(date)
    } else {
        Err(error())
    }
}

/// An amount in milliunits (SPEC 3.7): an optional sign, then digits with at
/// most one `.` or `,` among them, rounded half away from zero beyond three
/// decimal places.
fn amount(value: &str) -> Result<i64, String> {
    let error = || format!("{value} isn't an amount from -1000000000 to 1000000000 units");
    let (negative, number) = match value.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, value.strip_prefix('+').unwrap_or(value)),
    };
    let (whole, fraction) = number.split_once(['.', ',']).unwrap_or((number, ""));
    let all_digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if !all_digits(whole) || !all_digits(fraction) || whole.is_empty() && fraction.is_empty() {
        return Err(error());
    }
    // Past 13 digits, the whole units alone are out of range.
    let whole = whole.trim_start_matches('0');
    if whole.len() > 13 {
        return Err(error());
    }
    let digit = |i| {
        fraction
            .as_bytes()
            .get(i)
            .map_or(0, |b| i64::from(b - b'0'))
    };
    let units: i64 = if whole.is_empty() {
        0
    } else {
        whole.parse().map_err(|_| error())?
    };
    let rounding = i64::from(digit(3) >= 5);
    let milliunits = units * 1000 + digit(0) * 100 + digit(1) * 10 + digit(2) + rounding;
    let milliunits = if negative { -milliunits } else { milliunits };
    if fields::is_amount(milliunits) {
        Ok(milliunits)
    } else {
        Err(error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts() {
        for (text, milliunits) in [
            ("-64.99", -64990),
            ("+1800", 1_800_000),
            ("12,5", 12500),
            (".5", 500),
            ("5.", 5000),
            ("-0", 0),
            ("0.0005", 1),
            ("-0.0005", -1),
            ("0.00049999", 0),
            ("1.23456", 1235),
            ("1000000000", 1_000_000_000_000),
            ("0001000000000.000", 1_000_000_000_000),
        ] {
            assert_eq!(amount(text), Ok(milliunits), "{text}");
        }
        for bad in [
            "1,234.56",
            "",
            "-",
            ".",
            "+-5",
            "1 000",
            "1e3",
            "1000000000.0005",
            "99999999999999999999",
            "١٢",
        ] {
            assert!(amount(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn dates() {
        assert_eq!(date("20261005120000.000[-5:EST]").unwrap(), "2026-10-05");
        assert_eq!(date("20261031").unwrap(), "2026-10-31");
        for bad in [
            "2026103",
            "20261032",
            "18991231",
            "2026-10-05",
            "２０２６1005",
            "202é1050",
            "2026é005",
        ] {
            assert!(date(bad).is_err(), "{bad}");
        }
    }

    fn read_tree(text: &str) -> Tree {
        parse(text).unwrap()
    }

    fn names(node: Node<'_>) -> Vec<&str> {
        node.children().map(Node::name).collect()
    }

    #[test]
    fn values() {
        let tree = read_tree("<A>AB<!-- x -->CD<?pi?>E<B>&lt;AT&T&#38;&#x26;&nbsp;&bogus;&#;&</B>");
        let root = tree.root();
        assert_eq!(root.value_of("A"), Some("ABCDE"));
        assert_eq!(root.value_of("B"), Some("<AT&T&&\u{A0}&bogus;&#;&"));
        assert_eq!(read_tree("<X>  \n</X>").root().value_of("X"), None);
    }

    #[test]
    fn elements_without_end_tags() {
        let tree = read_tree("<S><T>DEBIT<D>2026<AMT>-1</S><n>x</N>");
        let s = tree.root().child("S").unwrap();
        assert_eq!(names(s), ["T", "D", "AMT"]);
        assert_eq!(s.value_of("T"), Some("DEBIT"));
        assert_eq!(s.value_of("AMT"), Some("-1"));
        assert_eq!(tree.root().value_of("N"), Some("x"));
        assert!(parse("<A></B>").is_err());
        assert!(parse("</A>").is_err());
    }

    #[test]
    fn text_after_an_element_without_end_tag_belongs_to_its_parent() {
        let tree = read_tree("<A>x<B>y<C>z</C>w<D>v</A>");
        let a = tree.root().child("A").unwrap();
        assert_eq!(names(a), ["B", "C", "D"]);
        assert_eq!(a.value(), Some("xw"));
        assert_eq!(a.value_of("B"), Some("y"));
        assert_eq!(a.value_of("C"), Some("z"));
        assert_eq!(a.value_of("D"), Some("v"));
        let tree = read_tree("<A>x<B>y<C>z");
        assert_eq!(names(tree.root()), ["A", "B", "C"]);
    }

    #[test]
    fn deep_nesting_reads_in_linear_time() {
        let unended = "<A>".repeat(200_000);
        assert_eq!(read_tree(&unended).elements.len(), 200_001);
        let nested = "<A>".repeat(200_000) + &"</A>".repeat(200_000);
        let tree = read_tree(&nested);
        assert_eq!(tree.root().children().count(), 1);
        drop(tree);
        assert_eq!(value(&"&".repeat(200_000)).unwrap().len(), 200_000);
    }

    #[test]
    fn first_present_element_counts() {
        let tree = read_tree("<A><N></N><N><X>x</X></N><N>one</N><N>two</N></A>");
        assert_eq!(tree.root().child("A").unwrap().value_of("N"), Some("one"));
        let tree = read_tree("<R><L>text</L><L><B>1</B></L></R>");
        let l = tree.root().child("R").unwrap().child("L").unwrap();
        assert_eq!(l.value_of("B"), Some("1"));
    }

    #[test]
    fn statements_and_transactions_are_never_absent() {
        let account = "<BANKACCTFROM><ACCTID>1</BANKACCTFROM>";
        for bad in [
            format!("<STMTRS></STMTRS><STMTRS>{account}</STMTRS>"),
            format!("<STMTRS>text</STMTRS><STMTRS>{account}</STMTRS>"),
            format!("<STMTRS>{account}<BANKTRANLIST><STMTTRN>x</STMTTRN></BANKTRANLIST></STMTRS>"),
        ] {
            assert!(statements(&read_tree(&bad)).is_err(), "{bad}");
        }
        let ignored_child = format!(
            "<STMTRS>{account}<LEDGERBAL><XFOO>1</XFOO></LEDGERBAL>\
             <LEDGERBAL><BALAMT>5</LEDGERBAL></STMTRS>"
        );
        let found = statements(&read_tree(&ignored_child)).unwrap();
        assert_eq!(found[0].ledger_balance, None);
    }

    #[test]
    fn elements_are_read_only_where_they_belong() {
        let file = "<STMTRS><BANKACCTFROM><ACCTID>1</BANKACCTFROM>\
            <BANKTRANLIST><STMTTRN><DTPOSTED>20261005<TRNAMT>1\
            <NAME>A<MEMO>inner</MEMO></NAME></STMTTRN></BANKTRANLIST>\
            <X><CCSTMTRS><CCACCTFROM><ACCTID>2</CCACCTFROM></CCSTMTRS></X>\
            </STMTRS>";
        let found = statements(&read_tree(file)).unwrap();
        let numbers: Vec<_> = found.iter().map(|s| s.account_number.as_str()).collect();
        assert_eq!(numbers, ["1", "2"]);
        let t = &found[0].transactions[0];
        assert_eq!((t.name.as_deref(), t.memo.as_deref()), (Some("A"), None));
    }

    #[test]
    fn encodings() {
        let sgml = b"\xEF\xBB\xBF \r\nOFXHEADER:100\r\nENCODING:USASCII\r\n\r\n<A>caf\xE9\x81";
        assert_eq!(decode(sgml).unwrap(), "<A>caf\u{E9}\u{81}");
        let utf8 = b"OFXHEADER:100\nENCODING:UTF-8\n<A>caf\xC3\xA9\xFF";
        assert_eq!(decode(utf8).unwrap(), "<A>caf\u{E9}\u{FFFD}");
        let xml = b"<?xml version=\"1.0\" encoding='cp1252'?><A>\xE9";
        assert!(decode(xml).unwrap().ends_with("<A>\u{E9}"));
        let xml = b"<?xml version=\"1.0\"?><A>\xC3\xA9";
        assert!(decode(xml).unwrap().ends_with("<A>\u{E9}"));
        assert!(decode(b"<?xml version=\"1.0\" encoding=\"UTF-16\"?><A>").is_err());
        assert!(decode(b"OFXHEADER:200\n<A>").is_err());
        assert!(decode(b"<OFX>").is_err());
    }
}
