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

/// An element: its name in uppercase, its value, and the elements it holds,
/// in order.
#[derive(Debug, Default)]
struct Element {
    name: String,
    /// Absent if empty.
    value: Option<String>,
    children: Vec<Element>,
}

impl Element {
    fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> {
        self.children.iter().filter(move |c| c.name == name)
    }

    /// Of the elements named `name` it holds directly, the first that holds
    /// other elements: SPEC 17.3's first that isn't absent, for an element
    /// whose elements SPEC 17.4 reads.
    fn child<'a>(&'a self, name: &'a str) -> Option<&'a Element> {
        self.children_named(name).find(|c| !c.children.is_empty())
    }

    /// Of the values of the elements named `name` it holds directly, the
    /// first that isn't absent (SPEC 17.3).
    fn value_of<'a>(&'a self, name: &'a str) -> Option<&'a str> {
        self.children_named(name).find_map(|c| c.value.as_deref())
    }
}

/// An element still open while the text is read.
#[derive(Default)]
struct Open {
    name: String,
    /// The text up to the first tag after its start tag: all it holds if it
    /// turns out to have no end tag.
    head: String,
    /// Whether a tag has come since its start tag.
    head_done: bool,
    /// The text it holds directly after `head`.
    tail: String,
    children: Vec<Element>,
}

impl Open {
    fn push_text(&mut self, text: &str) {
        if self.head_done {
            self.tail.push_str(text);
        } else {
            self.head.push_str(text);
        }
    }

    /// The element, closed by its own end tag: it holds everything since its
    /// start tag.
    fn closed(mut self) -> Element {
        self.head.push_str(&self.tail);
        Element {
            name: self.name,
            value: value(&self.head),
            children: self.children,
        }
    }
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
fn parse(text: &str) -> Result<Element, String> {
    // The root, which holds the top-level elements, is never closed by a tag.
    let mut stack = vec![Open::default()];
    for token in tokens(text) {
        match token {
            Token::Text(t) => top(&mut stack).push_text(t),
            Token::Start(name) => {
                top(&mut stack).head_done = true;
                stack.push(Open {
                    name: name.to_ascii_uppercase(),
                    ..Open::default()
                });
            }
            Token::End(name) => {
                top(&mut stack).head_done = true;
                let index = stack
                    .iter()
                    .rposition(|o| o.name.eq_ignore_ascii_case(name))
                    .filter(|&i| i > 0)
                    .ok_or_else(|| format!("the end tag </{name}> closes no open element"))?;
                while stack.len() > index + 1 {
                    close_unended(&mut stack);
                }
                let element = stack.pop().expect("the index is on the stack").closed();
                top(&mut stack).children.push(element);
            }
        }
    }
    while stack.len() > 1 {
        close_unended(&mut stack);
    }
    Ok(stack.pop().expect("the root stays on the stack").closed())
}

fn top(stack: &mut [Open]) -> &mut Open {
    stack.last_mut().expect("the root stays on the stack")
}

/// Closes the innermost open element, which has no end tag of its own: it
/// holds only its `head`, and what came after that belongs to its parent.
fn close_unended(stack: &mut Vec<Open>) {
    let open = stack.pop().expect("the caller leaves the root");
    let parent = top(stack);
    parent.children.push(Element {
        name: open.name,
        value: value(&open.head),
        children: Vec::new(),
    });
    parent.children.extend(open.children);
    parent.tail.push_str(&open.tail);
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
        rest = &rest[amp..];
        let reference = rest[1..]
            .find(';')
            .and_then(|semi| Some((reference(&rest[1..=semi])?, semi + 2)));
        let (c, len) = reference.unwrap_or(('&', 1));
        out.push(c);
        rest = &rest[len..];
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

/// The statements in the tree, in the order they appear.
fn statements(root: &Element) -> Result<Vec<Statement>, String> {
    let mut found = Vec::new();
    find_statements(root, &mut found);
    if found.is_empty() {
        return Err("the file holds no statements".into());
    }
    found.into_iter().map(statement).collect()
}

/// Statements are found wherever they are, even inside other statements, in
/// the order their start tags appear (SPEC 17.4).
fn find_statements<'a>(element: &'a Element, found: &mut Vec<&'a Element>) {
    for child in &element.children {
        if matches!(child.name.as_str(), "STMTRS" | "CCSTMTRS") {
            found.push(child);
        }
        find_statements(child, found);
    }
}

fn statement(s: &Element) -> Result<Statement, String> {
    let (kind, account_tag) = if s.name == "STMTRS" {
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

fn transaction(t: &Element) -> Result<BankTransaction, String> {
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

    fn tree(text: &str) -> Element {
        parse(text).unwrap()
    }

    #[test]
    fn values() {
        let root = tree("<A>AB<!-- x -->CD<?pi?>E<B>&lt;AT&T&#38;&#x26;&nbsp;&bogus;</B>");
        let a = &root.children[0];
        assert_eq!(a.value.as_deref(), Some("ABCDE"));
        assert_eq!(root.value_of("B"), Some("<AT&T&&\u{A0}&bogus;"));
        assert_eq!(tree("<X>  \n</X>").children[0].value, None);
    }

    #[test]
    fn elements_without_end_tags() {
        let root = tree("<S><T>DEBIT<D>2026<AMT>-1</S><n>x</N>");
        let s = root.child("S").unwrap();
        let names: Vec<_> = s.children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["T", "D", "AMT"]);
        assert_eq!(s.value_of("T"), Some("DEBIT"));
        assert_eq!(s.value_of("AMT"), Some("-1"));
        assert_eq!(root.value_of("N"), Some("x"));
        assert!(parse("<A></B>").is_err());
        assert!(parse("</A>").is_err());
    }

    #[test]
    fn first_present_element_counts() {
        let root = tree("<A><N></N><N><X>x</X></N><N>one</N><N>two</N></A>");
        assert_eq!(root.child("A").unwrap().value_of("N"), Some("one"));
        let root = tree("<R><L>text</L><L><B>1</B></L></R>");
        let l = root.child("R").unwrap().child("L").unwrap();
        assert_eq!(l.value_of("B"), Some("1"));
    }

    #[test]
    fn elements_are_read_only_where_they_belong() {
        let file = "<STMTRS><BANKACCTFROM><ACCTID>1</BANKACCTFROM>\
            <BANKTRANLIST><STMTTRN><DTPOSTED>20261005<TRNAMT>1\
            <NAME>A<MEMO>inner</MEMO></NAME></STMTTRN></BANKTRANLIST>\
            <X><CCSTMTRS><CCACCTFROM><ACCTID>2</CCACCTFROM></CCSTMTRS></X>\
            </STMTRS>";
        let found = statements(&tree(file)).unwrap();
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
