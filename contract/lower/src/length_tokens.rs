//! Length tokens written as their values where the theme cannot bind one:
//! grid tracks, which the kernel's track parser reads, by the token's name;
//! and custom property declarations, which the kernel holds as trees, and a
//! `var()`'s fallback, which a row reads at run time, by the token's
//! `var()`, which the loader leaves as written there (LLP 1123 D5).

use crate::LowerError;
use contract_syntax::{Attr, Expr, TemplatePart};
use exact_kernel::StyleId;
use std::borrow::Cow;

/// A grid track list with each length token it names written as the token's
/// value (`minmax(card-min-width, 1fr)`, the loader having written
/// `var(--card-min-width)` as the name), as a browser substitutes `var()` before
/// it parses the tracks (CSS Variables 1 §3). Only a token with one value in
/// every app mode is written in; the kernel's track parser refuses any other.
pub(crate) fn grid_tokens(a: &Attr, rows: &[StyleId]) -> Option<Attr> {
    if !rows
        .iter()
        .any(|r| matches!(r, StyleId::GridTemplateColumns | StyleId::GridTemplateRows))
    {
        return None;
    }
    let mut out = a.clone();
    texts(&mut out.value, &mut |text, place| {
        written_tokens(text, place == Place::AfterHole, None)
    })
    .then_some(out)
}

/// A custom property's declared value `e` with each design token in it
/// written as its value where the token is a length with one value in every
/// app mode, as a length row reads a token: through `var()`, which the
/// loader leaves as written here (LLP 1123 D5), or by its name, where the
/// text is literal whole; whether any was. A template whose holes are all
/// literal once a component is expanded is that literal. In a template with
/// a hole computed at run time only a `var()` is a token, since the hole's
/// text is unknown here. `varying` takes the first length token with more
/// than one value, which nothing binds where a value is read at run time.
pub(crate) fn call_tokens(e: &mut Expr, varying: &mut Option<String>) -> bool {
    fold_literal_templates(e);
    texts(e, &mut |text, place| {
        let explicit = token_calls(text, varying);
        match place {
            Place::Whole => written_tokens(text, false, Some(varying)) | explicit,
            _ => explicit,
        }
    })
}

/// Each template in `e`, through choices, bindings and typed values, whose
/// holes are all literal strings, written as the one string it makes.
fn fold_literal_templates(e: &mut Expr) {
    let literal = |e: &Expr| match e {
        Expr::Str(s, _) => Some(s.clone()),
        Expr::Typed(inner, ..) => match &**inner {
            Expr::Str(s, _) => Some(s.clone()),
            _ => None,
        },
        _ => None,
    };
    match e {
        Expr::Template(parts, span) => {
            // A hole that is itself a literal template is its literal first
            for part in parts.iter_mut() {
                if let TemplatePart::Expr(hole) = part {
                    fold_literal_templates(hole);
                }
            }
            let mut text = String::new();
            for part in parts.iter() {
                match part {
                    TemplatePart::Text(t) => text.push_str(t),
                    TemplatePart::Expr(hole) => match literal(hole) {
                        Some(s) => text.push_str(&s),
                        None => return,
                    },
                }
            }
            *e = Expr::Str(text, *span);
        }
        Expr::Ternary(_, yes, no, _)
        | Expr::Match {
            some: yes,
            none: no,
            ..
        } => {
            fold_literal_templates(yes);
            fold_literal_templates(no);
        }
        Expr::Let { body, .. } | Expr::Typed(body, ..) => fold_literal_templates(body),
        _ => {}
    }
}

/// Each `var()` of a token [`call_tokens`] writes, over a length row's value,
/// whose token calls left as written stand in a runtime fallback: elsewhere the loader wrote each as
/// its name, which the theme binds. A token in a fallback that varies by mode
/// is refused. A row's attribute goes through this before a box shorthand
/// splits its sides, so each side's fallback is written too.
pub(crate) fn fallback_tokens<'a>(
    a: &'a Attr,
    rows: &[StyleId],
) -> Result<Cow<'a, Attr>, LowerError> {
    if !rows
        .iter()
        .all(|r| r.codec() == exact_kernel::StyleCodec::Dimension)
    {
        return Ok(Cow::Borrowed(a));
    }
    let mut out = a.clone();
    let mut varying = None;
    let changed = texts(&mut out.value, &mut |text, _| {
        token_calls(text, &mut varying)
    });
    if let Some(token) = varying {
        return Err(LowerError {
            id: "lower-attr-value",
            message: format!("`{}`: the design token `{token}` differs between the app's modes, and a custom property's fallback is read at run time, where nothing binds it; give the property a value, or write a fixed length as its fallback", a.name),
            span: a.span,
        });
    }
    Ok(match changed {
        true => Cow::Owned(out),
        false => Cow::Borrowed(a),
    })
}

/// `text` with each `var()` of a length token that has one value in every
/// app mode written as the value; whether any was. A call in a comment, of a
/// host's name (`--exact-`, LLP 1081 D5) or of a name no length token has
/// stays.
fn token_calls(text: &mut String, varying: &mut Option<String>) -> bool {
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let (mut at, mut from, mut changed) = (0, 0, false);
    let name_char = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.');
    while let Some(start) = lower[from..].find("var(").map(|i| from + i) {
        from = start + 4;
        if inside_comment(text, start) || text[..start].ends_with(name_char) {
            continue;
        }
        let rest = text[from..].trim_start();
        let Some(name) = rest.strip_prefix("--") else {
            continue;
        };
        let name = &name[..name.find(|c| !name_char(c)).unwrap_or(name.len())];
        let Some(close) = call_end(text, from) else {
            break;
        };
        if name.starts_with("exact-") {
            continue;
        }
        match crate::theme::measure_written(name) {
            Some((exact_forma::Kind::Length, values)) if values.len() == 1 => {
                out.push_str(&text[at..start]);
                out.push_str(&values[0]);
                (at, from, changed) = (close + 1, close + 1, true);
            }
            Some((exact_forma::Kind::Length, _)) => {
                varying.get_or_insert_with(|| name.to_string());
            }
            _ => {}
        }
    }
    out.push_str(&text[at..]);
    *text = out;
    changed
}

/// The byte of the `)` that closes a function whose arguments start at
/// `from`, past nested parentheses, strings and comments.
fn call_end(text: &str, from: usize) -> Option<usize> {
    let (mut depth, mut quote, mut i) = (0usize, None, from);
    while let Some(c) = text[i..].chars().next() {
        if quote.is_none() {
            if let Some(past) = comment_end(text, i) {
                i = past;
                continue;
            }
        }
        match (quote, c) {
            // An escape takes the whole character after it
            (Some(_), '\\') => {
                i += 1;
                if let Some(escaped) = text[i..].chars().next() {
                    i += escaped.len_utf8();
                }
                continue;
            }
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '(') => depth += 1,
            (None, ')') if depth == 0 => return Some(i),
            (None, ')') => depth -= 1,
            _ => {}
        }
        i += c.len_utf8();
    }
    None
}

/// Where a literal text stands in a value.
#[derive(Clone, Copy, PartialEq)]
enum Place {
    /// The whole value.
    Whole,
    /// A template's text just after a hole.
    AfterHole,
    /// Any other part of a template.
    Part,
}

/// Each literal text of `e`, in either arm of a choice, under a binding and
/// between a template's holes, passed to `f` with where it stands; whether
/// `f` changed any.
fn texts(e: &mut Expr, f: &mut dyn FnMut(&mut String, Place) -> bool) -> bool {
    texts_in(e, f, false)
}

/// [`texts`], `inside` a template's hole, where even a whole string is one
/// part of the value.
fn texts_in(e: &mut Expr, f: &mut dyn FnMut(&mut String, Place) -> bool, inside: bool) -> bool {
    match e {
        Expr::Str(text, _) => f(text, if inside { Place::Part } else { Place::Whole }),
        Expr::Template(parts, _) => {
            let (mut changed, mut place) = (false, Place::Part);
            for part in parts {
                changed |= match part {
                    TemplatePart::Text(text) => f(text, place),
                    TemplatePart::Expr(e) => texts_in(e, f, true),
                };
                place = match part {
                    TemplatePart::Expr(_) => Place::AfterHole,
                    TemplatePart::Text(_) => Place::Part,
                };
            }
            changed
        }
        Expr::Ternary(_, yes, no, _)
        | Expr::Match {
            some: yes,
            none: no,
            ..
        } => texts_in(yes, f, inside) | texts_in(no, f, inside),
        Expr::Let { body, .. } => texts_in(body, f, inside),
        Expr::Typed(body, ..) => texts_in(body, f, inside),
        _ => false,
    }
}

/// `text` with each word that names a length token with one value in every
/// app mode written as the value, as grid tracks take one; whether any was.
/// A word that is a function's name, a custom
/// property's (after `-`), the variable `env()` names or a number's unit is
/// no token, and a comment is copied as it stands, its words none. `held`:
/// a template's hole stands just before `text`, so a CSS unit that starts it
/// is the unit of the number the hole makes (`${n}px`); any other word there
/// may still name a token, since the hole may be text.
fn written_tokens(text: &mut String, held: bool, varying: Option<&mut Option<String>>) -> bool {
    let mut varying = varying;
    let mut out = String::with_capacity(text.len());
    let (mut changed, mut env, mut i) = (false, false, 0);
    // What stands since the last word, comments aside
    let mut between = String::new();
    while let Some(c) = text[i..].chars().next() {
        if let Some(past) = comment_end(text, i) {
            out.push_str(&text[i..past]);
            // A comment separates what stands on either side, as white space does
            between.push(' ');
            i = past;
            continue;
        }
        // A word starts with a letter or `_`, so `--_pad` is one name after `--`
        if !c.is_ascii_alphabetic() && c != '_' {
            out.push(c);
            between.push(c);
            i += c.len_utf8();
            continue;
        }
        let end = text[i..]
            .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
            .map_or(text.len(), |e| i + e);
        let word = &text[i..end];
        // A word that follows a digit is the number's unit (`1fr`, `9.5rem`)
        let unit = between.ends_with(|c: char| c.is_ascii_digit() || c == '.')
            || held && i == 0 && is_unit(word);
        let function = text[end..].starts_with('(');
        let variable = env && between.trim() == "(";
        let named = between.ends_with('-') || function || variable;
        match crate::theme::measure_written(word).filter(|_| !unit && !named) {
            Some((exact_forma::Kind::Length, values)) if values.len() == 1 => {
                out.push_str(&values[0]);
                changed = true;
            }
            Some((exact_forma::Kind::Length, _)) => {
                if let Some(v) = varying.as_deref_mut() {
                    v.get_or_insert_with(|| word.to_string());
                }
                out.push_str(word);
            }
            _ => out.push_str(word),
        }
        env = function && word.eq_ignore_ascii_case("env");
        between.clear();
        i = end;
    }
    *text = out;
    changed
}

/// Whether `word` is a CSS unit a number takes (CSS Values 4 §6, §7).
fn is_unit(word: &str) -> bool {
    const UNITS: &[&str] = &[
        "px", "cm", "mm", "q", "in", "pt", "pc", "em", "rem", "ex", "rex", "ch", "rch", "cap",
        "rcap", "ic", "ric", "lh", "rlh", "vw", "vh", "vi", "vb", "vmin", "vmax", "svw", "svh",
        "svi", "svb", "svmin", "svmax", "lvw", "lvh", "lvi", "lvb", "lvmin", "lvmax", "dvw", "dvh",
        "dvi", "dvb", "dvmin", "dvmax", "cqw", "cqh", "cqi", "cqb", "cqmin", "cqmax", "fr", "deg",
        "grad", "rad", "turn", "s", "ms", "hz", "khz", "dpi", "dpcm", "dppx", "x",
    ];
    UNITS.iter().any(|u| word.eq_ignore_ascii_case(u))
}

/// Where a comment that opens at `i` ends, past its `*/` (or at the text's
/// end); `None` where none opens there.
fn comment_end(text: &str, i: usize) -> Option<usize> {
    text[i..].starts_with("/*").then(|| {
        text[i + 2..]
            .find("*/")
            .map_or(text.len(), |e| i + 2 + e + 2)
    })
}

/// Whether byte `at` of `text` is inside a comment.
fn inside_comment(text: &str, at: usize) -> bool {
    let mut i = 0;
    while let Some(start) = text[i..].find("/*").map(|s| i + s) {
        if start >= at {
            return false;
        }
        let end = comment_end(text, start).unwrap_or(text.len());
        if at < end {
            return true;
        }
        i = end;
    }
    false
}
