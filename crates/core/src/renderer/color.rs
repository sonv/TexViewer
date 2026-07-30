//! Safe LaTeX/xcolor-to-CSS color resolution for regular text.
//!
//! The renderer never copies an arbitrary color token into an HTML `style`
//! attribute.  Every supported model is parsed into an RGB triple first, so
//! malformed or hostile input simply produces no explicit color.

use std::cell::RefCell;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rgb {
    r: u8,
    g: u8,
    b: u8,
}

impl Rgb {
    fn css(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
    }

    fn mix(self, other: Self, self_percent: f64) -> Self {
        let weight = (self_percent / 100.0).clamp(0.0, 1.0);
        let channel =
            |a: u8, b: u8| (f64::from(a) * weight + f64::from(b) * (1.0 - weight)).round() as u8;
        Self {
            r: channel(self.r, other.r),
            g: channel(self.g, other.g),
            b: channel(self.b, other.b),
        }
    }
}

#[derive(Default)]
struct ColorRegistry {
    colors: HashMap<String, Rgb>,
}

thread_local! {
    static COLORS: RefCell<ColorRegistry> = RefCell::new(ColorRegistry::default());
}

const MAX_COLOR_DECLARATIONS: usize = 512;
const MAX_MIX_DEPTH: usize = 16;

pub(super) fn install(raw_preamble: &str) {
    let mut registry = ColorRegistry::default();
    install_builtin_colors(&mut registry.colors);

    let source = crate::macros::strip_line_comments(raw_preamble);
    let bytes = source.as_bytes();
    let mut declarations = 0usize;
    let mut hidden_ranges = Vec::<(usize, usize)>::new();
    let mut conditionals = crate::parser::ConditionalLookup::default();
    let mut i = 0usize;
    while i < bytes.len() && declarations < MAX_COLOR_DECLARATIONS {
        if let Some(resume) = crate::parser::scheduled_hidden_resume(i, &mut hidden_ranges) {
            i = resume;
            continue;
        }
        if bytes[i] != b'\\' {
            i += utf8_step(&source, i);
            continue;
        }
        let word_start = i + 1;
        let mut word_end = word_start;
        while word_end < bytes.len()
            && (bytes[word_end].is_ascii_alphabetic() || bytes[word_end] == b'@')
        {
            word_end += 1;
        }
        if word_end == word_start {
            i = if word_start < bytes.len() {
                word_start
                    + source[word_start..]
                        .chars()
                        .next()
                        .map_or(1, char::len_utf8)
            } else {
                bytes.len()
            };
            continue;
        }
        let command = &source[word_start..word_end];
        if command == "iffalse" {
            i = conditionals.false_branch_resume(&source, i, word_end);
            continue;
        }
        if command == "iftrue" {
            if let Some(range) = conditionals.true_branch_else_range(&source, i, word_end) {
                hidden_ranges.push(range);
            }
            i = word_end;
            continue;
        }
        if let Some(end) = stored_definition_end(&source, i, command, word_end) {
            i = end;
            continue;
        }
        if crate::parser::is_inline_literal_command(command) {
            i = crate::parser::inline_literal_payload(&source, command, word_end)
                .map(|(_, end)| end)
                .unwrap_or(bytes.len());
            continue;
        }
        match command {
            "definecolor" | "providecolor" => {
                declarations += 1;
                let Some((name, after_name)) = read_group(&source, word_end) else {
                    i = word_end;
                    continue;
                };
                let Some((model, after_model)) = read_group(&source, after_name) else {
                    i = after_name;
                    continue;
                };
                let Some((spec, after_spec)) = read_group(&source, after_model) else {
                    i = after_model;
                    continue;
                };
                let key = normalize_name(name);
                let is_provide = command == "providecolor";
                if !key.is_empty() && (!is_provide || !registry.colors.contains_key(&key)) {
                    if let Some(rgb) = resolve_model(&registry, Some(model), spec, 0) {
                        registry.colors.insert(key, rgb);
                    }
                }
                i = after_spec;
            }
            "colorlet" => {
                declarations += 1;
                let Some((name, after_name)) = read_group(&source, word_end) else {
                    i = word_end;
                    continue;
                };
                let mut spec_start = skip_space(&source, after_name);
                // xcolor also accepts an optional target model here. The
                // preview resolves the final source expression, so the target
                // storage model itself has no visual consequence.
                if bytes.get(spec_start) == Some(&b'[') {
                    if let Some((_, after_model)) = read_delimited(&source, spec_start, b'[', b']')
                    {
                        spec_start = after_model;
                    }
                }
                let Some((spec, after_spec)) = read_group(&source, spec_start) else {
                    i = spec_start;
                    continue;
                };
                let key = normalize_name(name);
                if !key.is_empty() {
                    if let Some(rgb) = resolve_model(&registry, None, spec, 0) {
                        registry.colors.insert(key, rgb);
                    }
                }
                i = after_spec;
            }
            _ => i = word_end,
        }
    }

    COLORS.with(|colors| *colors.borrow_mut() = registry);
}

/// Resolve a LaTeX color model/specification into one canonical CSS color.
pub(super) fn resolve_css(model: Option<&str>, spec: &str) -> Option<String> {
    COLORS.with(|colors| resolve_model(&colors.borrow(), model, spec, 0).map(Rgb::css))
}

fn resolve_model(
    registry: &ColorRegistry,
    model: Option<&str>,
    spec: &str,
    depth: usize,
) -> Option<Rgb> {
    if depth >= MAX_MIX_DEPTH || spec.len() > 256 {
        return None;
    }
    let spec = spec.trim();
    match model.map(str::trim) {
        Some("RGB") => {
            let values = parse_numbers(spec, 3)?;
            if values.iter().all(|value| (0.0..=255.0).contains(value)) {
                Some(Rgb {
                    r: values[0].round() as u8,
                    g: values[1].round() as u8,
                    b: values[2].round() as u8,
                })
            } else {
                None
            }
        }
        Some(model) if model.eq_ignore_ascii_case("HTML") => parse_hex(spec),
        Some("rgb") => {
            let values = parse_unit_values(spec, 3)?;
            Some(Rgb {
                r: unit_channel(values[0]),
                g: unit_channel(values[1]),
                b: unit_channel(values[2]),
            })
        }
        Some(model) if model.eq_ignore_ascii_case("gray") => {
            let value = parse_unit_values(spec, 1)?[0];
            let channel = unit_channel(value);
            Some(Rgb {
                r: channel,
                g: channel,
                b: channel,
            })
        }
        Some(model) if model.eq_ignore_ascii_case("cmyk") => {
            let values = parse_unit_values(spec, 4)?;
            let [c, m, y, k] = [values[0], values[1], values[2], values[3]];
            Some(Rgb {
                r: unit_channel(1.0 - (c + k).min(1.0)),
                g: unit_channel(1.0 - (m + k).min(1.0)),
                b: unit_channel(1.0 - (y + k).min(1.0)),
            })
        }
        Some(model)
            if model.eq_ignore_ascii_case("named") || model.eq_ignore_ascii_case("natural") =>
        {
            resolve_named(registry, spec, depth + 1)
        }
        Some(_) => None,
        None => resolve_named(registry, spec, depth + 1),
    }
}

fn resolve_named(registry: &ColorRegistry, spec: &str, depth: usize) -> Option<Rgb> {
    let mut parts = spec.split('!');
    let first = parts.next()?.trim();
    let mut color = if let Some(hex) = first.strip_prefix('#') {
        parse_hex(hex)?
    } else {
        *registry.colors.get(&normalize_name(first))?
    };

    while let Some(percent) = parts.next() {
        let percent = if percent.trim().is_empty() {
            50.0
        } else {
            percent.trim().parse::<f64>().ok()?
        };
        if !(0.0..=100.0).contains(&percent) {
            return None;
        }
        let other_name = parts.next().unwrap_or("white").trim();
        let other = if other_name.is_empty() {
            *registry.colors.get("white")?
        } else {
            resolve_model(registry, None, other_name, depth + 1)?
        };
        color = color.mix(other, percent);
    }
    Some(color)
}

fn parse_numbers(spec: &str, expected: usize) -> Option<Vec<f64>> {
    let values: Vec<f64> = spec
        .split(',')
        .map(str::trim)
        .map(str::parse::<f64>)
        .collect::<Result<_, _>>()
        .ok()?;
    (values.len() == expected && values.iter().all(|value| value.is_finite())).then_some(values)
}

fn parse_unit_values(spec: &str, expected: usize) -> Option<Vec<f64>> {
    let values = parse_numbers(spec, expected)?;
    values
        .iter()
        .all(|value| (0.0..=1.0).contains(value))
        .then_some(values)
}

fn unit_channel(value: f64) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn parse_hex(spec: &str) -> Option<Rgb> {
    let hex = spec.trim().trim_start_matches('#');
    match hex.len() {
        3 if hex.bytes().all(|byte| byte.is_ascii_hexdigit()) => {
            let r = u8::from_str_radix(&hex[0..1].repeat(2), 16).ok()?;
            let g = u8::from_str_radix(&hex[1..2].repeat(2), 16).ok()?;
            let b = u8::from_str_radix(&hex[2..3].repeat(2), 16).ok()?;
            Some(Rgb { r, g, b })
        }
        6 if hex.bytes().all(|byte| byte.is_ascii_hexdigit()) => Some(Rgb {
            r: u8::from_str_radix(&hex[0..2], 16).ok()?,
            g: u8::from_str_radix(&hex[2..4], 16).ok()?,
            b: u8::from_str_radix(&hex[4..6], 16).ok()?,
        }),
        _ => None,
    }
}

fn read_group(source: &str, start: usize) -> Option<(&str, usize)> {
    read_delimited(source, skip_space(source, start), b'{', b'}')
}

fn read_delimited(source: &str, start: usize, open: u8, close: u8) -> Option<(&str, usize)> {
    let end = crate::parser::tex_group_end(source, start, open, close)?;
    Some((&source[start + 1..end - 1], end))
}

fn skip_space(source: &str, mut i: usize) -> usize {
    let bytes = source.as_bytes();
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

fn stored_definition_end(
    source: &str,
    command_start: usize,
    command: &str,
    after_word: usize,
) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut i = after_word;
    if bytes.get(i) == Some(&b'*') {
        i += 1;
    }
    i = skip_space(source, i);

    if matches!(command, "def" | "gdef" | "edef" | "xdef") {
        while i < bytes.len() {
            if bytes[i] == b'{' {
                return crate::parser::tex_group_end(source, i, b'{', b'}');
            }
            if bytes[i] == b'%' {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            i += utf8_step(source, i);
        }
        return Some(bytes.len());
    }

    let latex_command = matches!(
        command,
        "newcommand" | "renewcommand" | "providecommand" | "DeclareRobustCommand"
    );
    let xparse_command = matches!(
        command,
        "NewDocumentCommand"
            | "RenewDocumentCommand"
            | "ProvideDocumentCommand"
            | "DeclareDocumentCommand"
    );
    let xparse_environment = matches!(
        command,
        "NewDocumentEnvironment"
            | "RenewDocumentEnvironment"
            | "ProvideDocumentEnvironment"
            | "DeclareDocumentEnvironment"
    );
    let environment_command = matches!(command, "newenvironment" | "renewenvironment");
    if !latex_command && !xparse_command && !xparse_environment && !environment_command {
        return None;
    }

    // Command/environment name: either `{\foo}` / `{name}` or one control
    // sequence token.
    i = if bytes.get(i) == Some(&b'{') {
        crate::parser::tex_group_end(source, i, b'{', b'}')?
    } else if bytes.get(i) == Some(&b'\\') {
        tex_control_token_end(source, i)
    } else {
        return Some(command_start.saturating_add(1));
    };
    i = skip_space(source, i);

    if xparse_command || xparse_environment {
        i = crate::parser::tex_group_end(source, i, b'{', b'}')?;
        i = skip_space(source, i);
        i = crate::parser::tex_group_end(source, i, b'{', b'}')?;
        if xparse_environment {
            i = skip_space(source, i);
            i = crate::parser::tex_group_end(source, i, b'{', b'}')?;
        }
        return Some(i);
    }

    // LaTeX command/environment optional arity and default.
    for _ in 0..2 {
        if bytes.get(i) != Some(&b'[') {
            break;
        }
        i = crate::parser::tex_group_end(source, i, b'[', b']')?;
        i = skip_space(source, i);
    }
    i = crate::parser::tex_group_end(source, i, b'{', b'}')?;
    if environment_command {
        i = skip_space(source, i);
        i = crate::parser::tex_group_end(source, i, b'{', b'}')?;
    }
    Some(i)
}

fn tex_control_token_end(source: &str, start: usize) -> usize {
    let bytes = source.as_bytes();
    let mut i = (start + 1).min(bytes.len());
    if bytes.get(i).is_some_and(u8::is_ascii_alphabetic) {
        while i < bytes.len() && (bytes[i].is_ascii_alphabetic() || bytes[i] == b'@') {
            i += 1;
        }
    } else if i < bytes.len() {
        i += source[i..].chars().next().map_or(1, char::len_utf8);
    }
    i
}

fn utf8_step(source: &str, i: usize) -> usize {
    if source.as_bytes()[i].is_ascii() {
        1
    } else {
        source[i..].chars().next().map_or(1, char::len_utf8)
    }
}

fn normalize_name(name: &str) -> String {
    name.trim().to_string()
}

fn install_builtin_colors(colors: &mut HashMap<String, Rgb>) {
    for (name, hex) in [
        ("black", "000000"),
        ("blue", "0000FF"),
        ("brown", "BF8040"),
        ("cyan", "00FFFF"),
        ("darkgray", "404040"),
        ("gray", "808080"),
        ("green", "00FF00"),
        ("lightgray", "BFBFBF"),
        ("lime", "BFFF00"),
        ("magenta", "FF00FF"),
        ("olive", "808000"),
        ("orange", "FF8000"),
        ("pink", "FFBFBF"),
        ("purple", "BF0040"),
        ("red", "FF0000"),
        ("teal", "008080"),
        ("violet", "800080"),
        ("white", "FFFFFF"),
        ("yellow", "FFFF00"),
        // Common `dvipsnames` names. TeX color names are case-sensitive, so
        // these intentionally coexist with the lowercase xcolor base names.
        ("GreenYellow", "ADFF2F"),
        ("Yellow", "FFFF00"),
        ("Goldenrod", "DAA520"),
        ("Dandelion", "F0E130"),
        ("Apricot", "FBB982"),
        ("Peach", "F7965A"),
        ("Melon", "F89E7B"),
        ("YellowOrange", "FFA500"),
        ("Orange", "FF8000"),
        ("BurntOrange", "F7921D"),
        ("Bittersweet", "C04F17"),
        ("RedOrange", "F26035"),
        ("Mahogany", "A9341F"),
        ("Maroon", "AF3235"),
        ("BrickRed", "B6321C"),
        ("Red", "FF0000"),
        ("OrangeRed", "FF4500"),
        ("RubineRed", "ED017D"),
        ("WildStrawberry", "EE2967"),
        ("Salmon", "F69289"),
        ("CarnationPink", "F282B4"),
        ("Magenta", "FF00FF"),
        ("VioletRed", "EF58A0"),
        ("Rhodamine", "EF559F"),
        ("Mulberry", "A93C93"),
        ("RedViolet", "A1246B"),
        ("Fuchsia", "8C368C"),
        ("Lavender", "F49EC4"),
        ("Thistle", "D883B7"),
        ("Orchid", "AF72B0"),
        ("DarkOrchid", "A4538A"),
        ("Purple", "A020F0"),
        ("Plum", "92278F"),
        ("Violet", "800080"),
        ("RoyalPurple", "613F99"),
        ("BlueViolet", "473992"),
        ("Periwinkle", "7977B8"),
        ("CadetBlue", "5F9F9F"),
        ("CornflowerBlue", "41B0E4"),
        ("MidnightBlue", "006795"),
        ("NavyBlue", "006EB8"),
        ("RoyalBlue", "0071BC"),
        ("Blue", "0000FF"),
        ("Cerulean", "00A2E3"),
        ("Cyan", "00FFFF"),
        ("ProcessBlue", "00B0F0"),
        ("SkyBlue", "46C5DD"),
        ("Turquoise", "00B4CE"),
        ("TealBlue", "00AEB3"),
        ("Aquamarine", "00B5BE"),
        ("BlueGreen", "00B3B8"),
        ("Emerald", "00A99D"),
        ("JungleGreen", "00A99A"),
        ("SeaGreen", "00A99D"),
        ("Green", "00FF00"),
        ("ForestGreen", "009B55"),
        ("PineGreen", "01796F"),
        ("LimeGreen", "8DC73E"),
        ("YellowGreen", "98CC70"),
        ("SpringGreen", "00A99D"),
        ("OliveGreen", "3C8031"),
        ("RawSienna", "974006"),
        ("Sepia", "671800"),
        ("Brown", "BF8040"),
        ("Tan", "D2B48C"),
        ("Gray", "808080"),
        ("Black", "000000"),
        ("White", "FFFFFF"),
    ] {
        if let Some(rgb) = parse_hex(hex) {
            colors.insert(name.to_string(), rgb);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_are_canonicalized_and_injection_is_rejected() {
        install("");
        assert_eq!(
            resolve_css(Some("HTML"), "ff8800").as_deref(),
            Some("#FF8800")
        );
        assert_eq!(
            resolve_css(Some("RGB"), "255,128,0").as_deref(),
            Some("#FF8000")
        );
        assert_eq!(
            resolve_css(Some("rgb"), "1,0.5,0").as_deref(),
            Some("#FF8000")
        );
        assert_eq!(resolve_css(Some("gray"), "0.5").as_deref(), Some("#808080"));
        assert_eq!(
            resolve_css(Some("cmyk"), "0,1,1,0").as_deref(),
            Some("#FF0000")
        );
        assert_eq!(resolve_css(None, "red;position:fixed"), None);
    }

    #[test]
    fn custom_definitions_aliases_and_mixes_resolve_in_order() {
        install(concat!(
            "\\definecolor{brand}{HTML}{336699}\n",
            "\\colorlet{softbrand}{brand!50!white}\n",
            "% \\definecolor{ignored}{HTML}{000000}\n",
        ));
        assert_eq!(resolve_css(None, "brand").as_deref(), Some("#336699"));
        assert_eq!(resolve_css(None, "softbrand").as_deref(), Some("#99B3CC"));
        assert_eq!(resolve_css(None, "red!50!blue").as_deref(), Some("#800080"));
        assert_eq!(resolve_css(None, "ignored"), None);
    }

    #[test]
    fn color_names_are_case_sensitive_and_unicode_input_cannot_panic() {
        install(concat!(
            "\\λ\n",
            "\\definecolor{Brand}{HTML}{FF0000}\n",
            "\\definecolor{brand}{HTML}{0000FF}\n",
            "\\providecolor{Red}{HTML}{123456}\n",
            "\\definecolor{broken}{named}{\\λ}\n",
        ));
        assert_eq!(resolve_css(None, "Brand").as_deref(), Some("#FF0000"));
        assert_eq!(resolve_css(None, "brand").as_deref(), Some("#0000FF"));
        assert_eq!(resolve_css(None, "Red").as_deref(), Some("#FF0000"));
        assert_eq!(resolve_css(None, "broken"), None);
    }

    #[test]
    fn dormant_color_declarations_do_not_override_live_preamble_state() {
        install(concat!(
            "\\definecolor{brand}{HTML}{0000FF}\n",
            "\\newcommand{\\never}{\\definecolor{brand}{HTML}{FF0000}}\n",
            "\\iffalse\n",
            "\\definecolor{brand}{HTML}{00FF00}\n",
            "\\fi\n",
            "\\iftrue\n",
            "\\colorlet{alias}{brand}\n",
            "\\else\n",
            "\\definecolor{brand}{HTML}{FFFFFF}\n",
            "\\fi\n",
        ));
        assert_eq!(resolve_css(None, "brand").as_deref(), Some("#0000FF"));
        assert_eq!(resolve_css(None, "alias").as_deref(), Some("#0000FF"));
    }

    #[test]
    fn xparse_environment_bodies_do_not_define_live_colors() {
        install(concat!(
            "\\NewDocumentEnvironment{stored}{}",
            "{\\definecolor{phantom}{HTML}{FF0000}}{}\n",
            "\\RenewDocumentEnvironment{stored}{m}",
            "{\\definecolor{also-phantom}{HTML}{00FF00}}{}\n",
        ));
        assert_eq!(resolve_css(None, "phantom"), None);
        assert_eq!(resolve_css(None, "also-phantom"), None);
    }
}
