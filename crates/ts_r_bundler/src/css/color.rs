//! Shorter colors for `--minify-syntax`, as observed in esbuild 0.28.2: in properties that
//! take a color, a color is printed in its shortest form: lowercase hex, `#rgb` / `#rgba`
//! when the digits pair up, no alpha when it is opaque, or a color keyword when the keyword
//! is strictly shorter (`#f00` is `red`, `blue` is `#00f`). `rgb()`, `rgba()`, `hsl()` and
//! `hsla()` with plain numbers become hex. Anything else (`var()`, `currentcolor`, invalid
//! hex) stays as written.

/// The CSS named colors (CSS Color Module Level 4), sorted by name.
const NAMED: &[(&str, u32)] = &[
    ("aliceblue", 0xf0f8ff),
    ("antiquewhite", 0xfaebd7),
    ("aqua", 0x00ffff),
    ("aquamarine", 0x7fffd4),
    ("azure", 0xf0ffff),
    ("beige", 0xf5f5dc),
    ("bisque", 0xffe4c4),
    ("black", 0x000000),
    ("blanchedalmond", 0xffebcd),
    ("blue", 0x0000ff),
    ("blueviolet", 0x8a2be2),
    ("brown", 0xa52a2a),
    ("burlywood", 0xdeb887),
    ("cadetblue", 0x5f9ea0),
    ("chartreuse", 0x7fff00),
    ("chocolate", 0xd2691e),
    ("coral", 0xff7f50),
    ("cornflowerblue", 0x6495ed),
    ("cornsilk", 0xfff8dc),
    ("crimson", 0xdc143c),
    ("cyan", 0x00ffff),
    ("darkblue", 0x00008b),
    ("darkcyan", 0x008b8b),
    ("darkgoldenrod", 0xb8860b),
    ("darkgray", 0xa9a9a9),
    ("darkgreen", 0x006400),
    ("darkgrey", 0xa9a9a9),
    ("darkkhaki", 0xbdb76b),
    ("darkmagenta", 0x8b008b),
    ("darkolivegreen", 0x556b2f),
    ("darkorange", 0xff8c00),
    ("darkorchid", 0x9932cc),
    ("darkred", 0x8b0000),
    ("darksalmon", 0xe9967a),
    ("darkseagreen", 0x8fbc8f),
    ("darkslateblue", 0x483d8b),
    ("darkslategray", 0x2f4f4f),
    ("darkslategrey", 0x2f4f4f),
    ("darkturquoise", 0x00ced1),
    ("darkviolet", 0x9400d3),
    ("deeppink", 0xff1493),
    ("deepskyblue", 0x00bfff),
    ("dimgray", 0x696969),
    ("dimgrey", 0x696969),
    ("dodgerblue", 0x1e90ff),
    ("firebrick", 0xb22222),
    ("floralwhite", 0xfffaf0),
    ("forestgreen", 0x228b22),
    ("fuchsia", 0xff00ff),
    ("gainsboro", 0xdcdcdc),
    ("ghostwhite", 0xf8f8ff),
    ("gold", 0xffd700),
    ("goldenrod", 0xdaa520),
    ("gray", 0x808080),
    ("green", 0x008000),
    ("greenyellow", 0xadff2f),
    ("grey", 0x808080),
    ("honeydew", 0xf0fff0),
    ("hotpink", 0xff69b4),
    ("indianred", 0xcd5c5c),
    ("indigo", 0x4b0082),
    ("ivory", 0xfffff0),
    ("khaki", 0xf0e68c),
    ("lavender", 0xe6e6fa),
    ("lavenderblush", 0xfff0f5),
    ("lawngreen", 0x7cfc00),
    ("lemonchiffon", 0xfffacd),
    ("lightblue", 0xadd8e6),
    ("lightcoral", 0xf08080),
    ("lightcyan", 0xe0ffff),
    ("lightgoldenrodyellow", 0xfafad2),
    ("lightgray", 0xd3d3d3),
    ("lightgreen", 0x90ee90),
    ("lightgrey", 0xd3d3d3),
    ("lightpink", 0xffb6c1),
    ("lightsalmon", 0xffa07a),
    ("lightseagreen", 0x20b2aa),
    ("lightskyblue", 0x87cefa),
    ("lightslategray", 0x778899),
    ("lightslategrey", 0x778899),
    ("lightsteelblue", 0xb0c4de),
    ("lightyellow", 0xffffe0),
    ("lime", 0x00ff00),
    ("limegreen", 0x32cd32),
    ("linen", 0xfaf0e6),
    ("magenta", 0xff00ff),
    ("maroon", 0x800000),
    ("mediumaquamarine", 0x66cdaa),
    ("mediumblue", 0x0000cd),
    ("mediumorchid", 0xba55d3),
    ("mediumpurple", 0x9370db),
    ("mediumseagreen", 0x3cb371),
    ("mediumslateblue", 0x7b68ee),
    ("mediumspringgreen", 0x00fa9a),
    ("mediumturquoise", 0x48d1cc),
    ("mediumvioletred", 0xc71585),
    ("midnightblue", 0x191970),
    ("mintcream", 0xf5fffa),
    ("mistyrose", 0xffe4e1),
    ("moccasin", 0xffe4b5),
    ("navajowhite", 0xffdead),
    ("navy", 0x000080),
    ("oldlace", 0xfdf5e6),
    ("olive", 0x808000),
    ("olivedrab", 0x6b8e23),
    ("orange", 0xffa500),
    ("orangered", 0xff4500),
    ("orchid", 0xda70d6),
    ("palegoldenrod", 0xeee8aa),
    ("palegreen", 0x98fb98),
    ("paleturquoise", 0xafeeee),
    ("palevioletred", 0xdb7093),
    ("papayawhip", 0xffefd5),
    ("peachpuff", 0xffdab9),
    ("peru", 0xcd853f),
    ("pink", 0xffc0cb),
    ("plum", 0xdda0dd),
    ("powderblue", 0xb0e0e6),
    ("purple", 0x800080),
    ("rebeccapurple", 0x663399),
    ("red", 0xff0000),
    ("rosybrown", 0xbc8f8f),
    ("royalblue", 0x4169e1),
    ("saddlebrown", 0x8b4513),
    ("salmon", 0xfa8072),
    ("sandybrown", 0xf4a460),
    ("seagreen", 0x2e8b57),
    ("seashell", 0xfff5ee),
    ("sienna", 0xa0522d),
    ("silver", 0xc0c0c0),
    ("skyblue", 0x87ceeb),
    ("slateblue", 0x6a5acd),
    ("slategray", 0x708090),
    ("slategrey", 0x708090),
    ("snow", 0xfffafa),
    ("springgreen", 0x00ff7f),
    ("steelblue", 0x4682b4),
    ("tan", 0xd2b48c),
    ("teal", 0x008080),
    ("thistle", 0xd8bfd8),
    ("tomato", 0xff6347),
    ("turquoise", 0x40e0d0),
    ("violet", 0xee82ee),
    ("wheat", 0xf5deb3),
    ("white", 0xffffff),
    ("whitesmoke", 0xf5f5f5),
    ("yellow", 0xffff00),
    ("yellowgreen", 0x9acd32),
];

/// Whether colors in property `name`'s value are shortened.
pub fn takes_color(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    matches!(
        n.as_str(),
        "color"
            | "background"
            | "background-color"
            | "border-color"
            | "border-top-color"
            | "border-right-color"
            | "border-bottom-color"
            | "border-left-color"
            | "border-block-start-color"
            | "border-block-end-color"
            | "border-inline-start-color"
            | "border-inline-end-color"
            | "box-shadow"
            | "caret-color"
            | "column-rule-color"
            | "fill"
            | "flood-color"
            | "lighting-color"
            | "outline-color"
            | "stop-color"
            | "stroke"
            | "text-decoration-color"
            | "text-emphasis-color"
    )
}

/// The shortest text for an RGBA color.
fn format(rgba: u32) -> String {
    let [r, g, b, a] = rgba.to_be_bytes();
    if a == 0xff {
        let rgb = rgba >> 8;
        let hex = if [r, g, b].iter().all(|c| c >> 4 == c & 15) {
            format!("#{:x}{:x}{:x}", r & 15, g & 15, b & 15)
        } else {
            format!("#{rgb:06x}")
        };
        if let Some((name, _)) = NAMED.iter().find(|(n, v)| *v == rgb && n.len() < hex.len()) {
            return (*name).to_owned();
        }
        return hex;
    }
    if [r, g, b, a].iter().all(|c| c >> 4 == c & 15) {
        format!("#{:x}{:x}{:x}{:x}", r & 15, g & 15, b & 15, a & 15)
    } else {
        format!("#{rgba:08x}")
    }
}

/// A `#` hex color's RGBA value (3, 4, 6 or 8 digits).
fn parse_hex(digits: &str) -> Option<u32> {
    if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(digits, 16).ok()?;
    // A digit doubled: `#abc` is `#aabbcc`.
    let nib = |i: u32| (((v >> (4 * i)) & 15) * 0x11) as u8;
    Some(match digits.len() {
        3 => u32::from_be_bytes([nib(2), nib(1), nib(0), 0xff]),
        4 => u32::from_be_bytes([nib(3), nib(2), nib(1), nib(0)]),
        6 => (v << 8) | 0xff,
        8 => v,
        _ => return None,
    })
}

/// A shorter form of the color keyword or `#hex` token `text`, if it is one.
pub fn shorten_token(text: &str) -> Option<String> {
    let rgba = if let Some(d) = text.strip_prefix('#') {
        parse_hex(d)?
    } else {
        let lower = text.to_ascii_lowercase();
        let i = NAMED
            .binary_search_by(|(n, _)| (*n).cmp(lower.as_str()))
            .ok()?;
        (NAMED[i].1 << 8) | 0xff
    };
    Some(format(rgba))
}

/// `rgb(...)` / `rgba(...)` / `hsl(...)` / `hsla(...)` from its function name (`rgb(`)
/// and its arguments (numbers and percentages, comma- or space-separated, `/` before
/// alpha), as the shortest color text.
pub fn shorten_function(name: &str, args: &[&str]) -> Option<String> {
    let name = name.trim_end_matches('(').to_ascii_lowercase();
    let (is_hsl, max) = match name.as_str() {
        "rgb" | "rgba" => (false, 4),
        "hsl" | "hsla" => (true, 4),
        _ => return None,
    };
    if args.len() < 3 || args.len() > max {
        return None;
    }
    let num = |s: &str| -> Option<(f64, bool)> {
        let (n, pct) = match s.strip_suffix('%') {
            Some(n) => (n, true),
            None => (s, false),
        };
        let v: f64 = n.parse().ok()?;
        v.is_finite().then_some((v, pct))
    };
    let alpha = match args.get(3) {
        None => 1.0,
        Some(a) => {
            let (v, pct) = num(a)?;
            if pct { v / 100.0 } else { v }
        }
    };
    let byte = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    let (r, g, b) = if is_hsl {
        let h = args[0].trim_end_matches("deg");
        let (h, hp) = num(h)?;
        let (s, sp) = num(args[1])?;
        let (l, lp) = num(args[2])?;
        if hp || !sp || !lp {
            return None;
        }
        let (r, g, b) = hsl_to_rgb(h.rem_euclid(360.0) / 360.0, s / 100.0, l / 100.0);
        (byte(r), byte(g), byte(b))
    } else {
        let mut c = [0u32; 3];
        for (i, a) in args[..3].iter().enumerate() {
            let (v, pct) = num(a)?;
            c[i] = if pct {
                byte(v / 100.0)
            } else {
                v.clamp(0.0, 255.0).round() as u32
            };
        }
        (c[0], c[1], c[2])
    };
    Some(format(r << 24 | g << 16 | b << 8 | byte(alpha)))
}

fn hsl_to_rgb(h: f64, s: f64, l: f64) -> (f64, f64, f64) {
    let s = s.clamp(0.0, 1.0);
    let l = l.clamp(0.0, 1.0);
    let q = if l <= 0.5 {
        l * (s + 1.0)
    } else {
        l + s - l * s
    };
    let p = l * 2.0 - q;
    let hue = |mut t: f64| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t * 6.0 < 1.0 {
            p + (q - p) * t * 6.0
        } else if t * 2.0 < 1.0 {
            q
        } else if t * 3.0 < 2.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    (hue(h + 1.0 / 3.0), hue(h), hue(h - 1.0 / 3.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted() {
        assert!(NAMED.windows(2).all(|w| w[0].0 < w[1].0));
    }

    #[test]
    fn shortest_forms() {
        let t = |s: &str| shorten_token(s).unwrap_or_else(|| s.to_owned());
        assert_eq!(t("blue"), "#00f");
        assert_eq!(t("BLUE"), "#00f");
        assert_eq!(t("#FF0000"), "red");
        assert_eq!(t("#AbCdEf"), "#abcdef");
        assert_eq!(t("#aabbccdd"), "#abcd");
        assert_eq!(t("#aabbccff"), "#abc");
        assert_eq!(t("#000080"), "navy");
        assert_eq!(t("LightGoldenRodYellow"), "#fafad2");
        assert_eq!(t("#ff000"), "#ff000");
        assert_eq!(t("currentcolor"), "currentcolor");
        assert_eq!(t("white"), "#fff");
        let f = |n: &str, a: &[&str]| shorten_function(n, a);
        assert_eq!(f("rgb(", &["255", "0", "0"]).as_deref(), Some("red"));
        assert_eq!(
            f("rgba(", &["255", "0", "0", "0.5"]).as_deref(),
            Some("#ff000080")
        );
        assert_eq!(f("rgba(", &["0", "0", "0", "0"]).as_deref(), Some("#0000"));
        assert_eq!(f("rgb(", &["100%", "0%", "0%"]).as_deref(), Some("red"));
        assert_eq!(f("hsl(", &["0", "100%", "50%"]).as_deref(), Some("red"));
        assert_eq!(f("rgb(", &["var(--x)", "0", "0"]), None);
    }
}
