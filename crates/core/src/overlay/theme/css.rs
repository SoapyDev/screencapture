//! Reads the colours out of a GTK stylesheet.
//!
//! `@define-color` lines (with `@name` aliases and colour functions) and the `@import`s that lead
//! to more of them. Pure text in, colours out; nothing here touches the disk.

use std::collections::HashMap;

use gpui::{Hsla, Rgba};

/// What a stylesheet declares.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Sheet {
    /// Files it pulls in, as written.
    pub imports: Vec<String>,
    /// `@define-color` name and value, in order (a later one replaces an earlier one).
    pub definitions: Vec<(String, String)>,
}

/// The statements of `css`: its imports and colour definitions.
#[must_use]
pub fn read_sheet(css: &str) -> Sheet {
    let mut sheet = Sheet::default();
    for statement in without_comments(css).split(';') {
        if let Some(path) = import_path(statement) {
            sheet.imports.push(path);
        } else if let Some(definition) = definition(statement) {
            sheet.definitions.push(definition);
        }
    }
    sheet
}

fn without_comments(css: &str) -> String {
    let mut kept = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find("/*") {
        kept.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        rest = after.find("*/").map_or("", |end| &after[end + 2..]);
    }
    kept.push_str(rest);
    kept
}

fn import_path(statement: &str) -> Option<String> {
    let (_, target) = statement.split_once("@import")?;
    let target = target.trim();
    let target = target
        .strip_prefix("url(")
        .map_or(target, |inner| inner.trim_end_matches(')'));
    let path = target.trim().trim_matches(['"', '\'']);
    (!path.is_empty()).then(|| path.to_owned())
}

fn definition(statement: &str) -> Option<(String, String)> {
    let (_, declared) = statement.split_once("@define-color")?;
    let (name, value) = declared.trim().split_once(char::is_whitespace)?;
    let value = value.trim();
    (!value.is_empty()).then(|| (name.to_owned(), value.to_owned()))
}

/// Every definition that comes out to a colour, by name. Ones that refer to something unknown
/// (or to each other in a circle) or use something not understood are left out.
#[must_use]
pub fn resolve(definitions: &[(String, String)]) -> HashMap<String, Hsla> {
    let mut known = HashMap::new();
    for _ in 0..=definitions.len() {
        let before = known.len();
        for (name, value) in definitions {
            let found = evaluate(value, &|alias| known.get(alias).copied());
            if let Some(colour) = found {
                known.insert(name.clone(), colour);
            }
        }
        if known.len() == before {
            break;
        }
    }
    known
}

enum Arg {
    Colour(Hsla),
    Number(f32),
}

struct Call {
    name: String,
    args: Vec<Arg>,
}

/// The colour `expression` makes: `#rrggbb`, `rgb()`, `rgba()`, `@alias` (through `lookup`),
/// `alpha()`, `mix()`, `shade()`, `lighter()`, `darker()`, `white`, `black`, `transparent`.
/// No recursion: calls nest on an explicit stack.
pub fn evaluate(expression: &str, lookup: &dyn Fn(&str) -> Option<Hsla>) -> Option<Hsla> {
    let mut open: Vec<Call> = Vec::new();
    let mut top_level: Vec<Arg> = Vec::new();
    for token in tokens(expression) {
        read_token(token, &mut open, &mut top_level, lookup)?;
    }
    match (open.is_empty(), top_level.as_slice()) {
        (true, [Arg::Colour(colour)]) => Some(*colour),
        _ => None,
    }
}

/// Takes `token` into the call being read (or the top level); `None` if it can't be a colour.
fn read_token(
    token: Token,
    open: &mut Vec<Call>,
    top_level: &mut Vec<Arg>,
    lookup: &dyn Fn(&str) -> Option<Hsla>,
) -> Option<()> {
    match token {
        Token::Comma => {}
        Token::Open(name) => open.push(Call {
            name,
            args: Vec::new(),
        }),
        Token::Close => {
            let call = open.pop()?;
            let made = apply(&call.name, &call.args)?;
            args_of(open, top_level).push(made);
        }
        Token::Value(text) => {
            let arg = value_arg(&text, lookup)?;
            args_of(open, top_level).push(arg);
        }
    }
    Some(())
}

fn args_of<'a>(open: &'a mut [Call], top_level: &'a mut Vec<Arg>) -> &'a mut Vec<Arg> {
    open.last_mut().map_or(top_level, |call| &mut call.args)
}

enum Token {
    Comma,
    Open(String),
    Close,
    Value(String),
}

fn tokens(expression: &str) -> Vec<Token> {
    let mut found = Vec::new();
    let mut rest = expression.trim_start();
    while let Some(next) = rest.chars().next() {
        let (token, length) = match next {
            ',' => (Token::Comma, 1),
            ')' => (Token::Close, 1),
            _ => word(rest),
        };
        found.push(token);
        rest = rest[length..].trim_start();
    }
    found
}

fn word(text: &str) -> (Token, usize) {
    let end = text.find([',', '(', ')']).unwrap_or(text.len());
    let word = text[..end].trim();
    if text[end..].starts_with('(') {
        (Token::Open(word.to_owned()), end + 1)
    } else {
        (Token::Value(word.to_owned()), end)
    }
}

fn value_arg(text: &str, lookup: &dyn Fn(&str) -> Option<Hsla>) -> Option<Arg> {
    if let Some(alias) = text.strip_prefix('@') {
        return lookup(alias).map(Arg::Colour);
    }
    if let Some(colour) = hex(text).or_else(|| named(text)) {
        return Some(Arg::Colour(colour));
    }
    number(text).map(Arg::Number)
}

fn number(text: &str) -> Option<f32> {
    let percent = text.strip_suffix('%');
    percent.map_or_else(
        || text.parse().ok(),
        |digits| digits.trim().parse::<f32>().ok().map(|v| v / 100.),
    )
}

fn named(text: &str) -> Option<Hsla> {
    match text {
        "white" => Some(gpui::white()),
        "black" => Some(gpui::black()),
        "transparent" => Some(gpui::transparent_black()),
        _ => None,
    }
}

fn hex(text: &str) -> Option<Hsla> {
    let digits = text.strip_prefix('#')?;
    let expanded: String = match digits.len() {
        3 | 4 => digits.chars().flat_map(|digit| [digit, digit]).collect(),
        6 | 8 => digits.to_owned(),
        _ => return None,
    };
    let channel = |at: usize| u8::from_str_radix(expanded.get(at..at + 2)?, 16).ok();
    let alpha = if expanded.len() == 8 { channel(6)? } else { 255 };
    Some(from_bytes(channel(0)?, channel(2)?, channel(4)?, alpha))
}

pub(super) fn from_bytes(red: u8, green: u8, blue: u8, alpha: u8) -> Hsla {
    let unit = |byte: u8| f32::from(byte) / 255.;
    Rgba {
        r: unit(red),
        g: unit(green),
        b: unit(blue),
        a: unit(alpha),
    }
    .into()
}

fn apply(name: &str, args: &[Arg]) -> Option<Arg> {
    let colour = match (name, args) {
        ("rgb", [Arg::Number(r), Arg::Number(g), Arg::Number(b)]) => channels(*r, *g, *b, 1.),
        ("rgba", [Arg::Number(r), Arg::Number(g), Arg::Number(b), Arg::Number(a)]) => {
            channels(*r, *g, *b, *a)
        }
        ("rgba" | "alpha", [Arg::Colour(base), Arg::Number(factor)]) => with_alpha(*base, name, *factor),
        ("mix", [Arg::Colour(a), Arg::Colour(b), Arg::Number(share)]) => mixed(*a, *b, *share),
        ("shade", [Arg::Colour(base), Arg::Number(factor)]) => shaded(*base, *factor),
        ("lighter", [Arg::Colour(base)]) => shaded(*base, 1.3),
        ("darker", [Arg::Colour(base)]) => shaded(*base, 0.7),
        _ => return None,
    };
    Some(Arg::Colour(colour))
}

fn channels(red: f32, green: f32, blue: f32, alpha: f32) -> Hsla {
    let unit = |value: f32| (value / 255.).clamp(0., 1.);
    Rgba {
        r: unit(red),
        g: unit(green),
        b: unit(blue),
        a: alpha.clamp(0., 1.),
    }
    .into()
}

/// `rgba(colour, a)` sets the opacity; `alpha(colour, f)` scales it.
fn with_alpha(base: Hsla, function: &str, factor: f32) -> Hsla {
    let alpha = if function == "alpha" {
        base.a * factor
    } else {
        factor
    };
    Hsla {
        a: alpha.clamp(0., 1.),
        ..base
    }
}

/// `share` of the way from `first` to `second`, channel by channel.
pub(super) fn mixed(first: Hsla, second: Hsla, share: f32) -> Hsla {
    let (a, b) = (Rgba::from(first), Rgba::from(second));
    let share = share.clamp(0., 1.);
    let blend = |from: f32, to: f32| (to - from).mul_add(share, from);
    Rgba {
        r: blend(a.r, b.r),
        g: blend(a.g, b.g),
        b: blend(a.b, b.b),
        a: blend(a.a, b.a),
    }
    .into()
}

fn shaded(base: Hsla, factor: f32) -> Hsla {
    Hsla {
        l: (base.l * factor).clamp(0., 1.),
        ..base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &str) -> Option<Hsla> {
        None
    }

    fn colour(text: &str) -> Hsla {
        evaluate(text, &none).expect("the test's colour is valid")
    }

    #[test]
    fn a_six_digit_hex_is_read() {
        assert_eq!(colour("#cba6f7"), from_bytes(0xcb, 0xa6, 0xf7, 255));
    }

    #[test]
    fn a_three_digit_hex_doubles_each_digit() {
        assert_eq!(colour("#fa0"), colour("#ffaa00"));
    }

    #[test]
    fn an_eight_digit_hex_carries_its_opacity() {
        assert_eq!(colour("#00000080"), from_bytes(0, 0, 0, 0x80));
    }

    #[test]
    fn rgb_takes_channels_out_of_255() {
        assert_eq!(colour("rgb(255, 0, 0)"), from_bytes(255, 0, 0, 255));
    }

    #[test]
    fn rgba_takes_its_opacity_as_the_last_number() {
        assert!((colour("rgba(0, 0, 0, 0.5)").a - 0.5).abs() < 0.001);
    }

    #[test]
    fn alpha_scales_the_opacity_of_a_colour() {
        assert!((colour("alpha(#ffffff, 0.25)").a - 0.25).abs() < 0.001);
    }

    #[test]
    fn mix_halfway_between_black_and_white_is_grey() {
        let grey = Rgba::from(colour("mix(black, white, 0.5)"));
        assert!((grey.r - 0.5).abs() < 0.001);
    }

    #[test]
    fn shade_scales_the_lightness() {
        let (shaded, expected) = (colour("shade(#808080, 0.5)").l, colour("#808080").l * 0.5);
        assert!((shaded - expected).abs() < 0.001);
    }

    #[test]
    fn calls_nest() {
        assert!((colour("alpha(mix(black, white, 0.5), 0.5)").a - 0.5).abs() < 0.001);
    }

    #[test]
    fn an_unknown_function_is_not_a_colour() {
        assert_eq!(evaluate("wobble(#fff)", &none), None);
    }

    #[test]
    fn an_unclosed_call_is_not_a_colour() {
        assert_eq!(evaluate("alpha(#fff, 0.5", &none), None);
    }

    #[test]
    fn an_alias_goes_through_the_lookup() {
        let red = from_bytes(255, 0, 0, 255);
        let lookup = |name: &str| (name == "accent").then_some(red);
        assert_eq!(evaluate("@accent", &lookup), Some(red));
    }

    #[test]
    fn a_sheet_lists_its_imports() {
        let sheet = read_sheet("@import url(\"dank-colors.css\");\n@import 'other.css';");
        assert_eq!(sheet.imports, ["dank-colors.css", "other.css"]);
    }

    #[test]
    fn a_sheet_lists_its_definitions_in_order() {
        let sheet = read_sheet("@define-color a #111111;\n@define-color b @a;");
        assert_eq!(sheet.definitions[1], ("b".to_owned(), "@a".to_owned()));
    }

    #[test]
    fn comments_hide_definitions() {
        let sheet = read_sheet("/* @define-color a #111111; */ @define-color b #222222;");
        assert_eq!(sheet.definitions.len(), 1);
    }

    #[test]
    fn a_definition_after_a_rule_is_still_found() {
        let sheet = read_sheet("row { color: red; }\n@define-color a #111111;");
        assert_eq!(sheet.definitions.len(), 1);
    }

    #[test]
    fn aliases_defined_before_what_they_point_at_resolve() {
        let definitions = [
            ("b".to_owned(), "@a".to_owned()),
            ("a".to_owned(), "#ffffff".to_owned()),
        ];
        assert_eq!(resolve(&definitions).get("b"), Some(&gpui::white()));
    }

    #[test]
    fn a_circle_of_aliases_resolves_to_nothing() {
        let definitions = [
            ("a".to_owned(), "@b".to_owned()),
            ("b".to_owned(), "@a".to_owned()),
        ];
        assert!(resolve(&definitions).is_empty());
    }

    #[test]
    fn a_later_definition_replaces_an_earlier_one() {
        let definitions = [
            ("a".to_owned(), "#000000".to_owned()),
            ("a".to_owned(), "#ffffff".to_owned()),
        ];
        assert_eq!(resolve(&definitions).get("a"), Some(&gpui::white()));
    }
}
