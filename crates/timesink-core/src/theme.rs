//! Colour themes: named colour "slots" and the built-in palettes.
//!
//! Screens never use raw colours for chrome (text, borders, highlights). They ask for a slot
//! ("the muted colour", "the warning colour") and the current theme decides the real RGB value,
//! so switching themes is only a matter of swapping which [`Theme`] is current.
//!
//! Each theme also carries its own list of accent colours for the chart bars (see
//! [`Theme::category_color`]), so the bars match the rest of the screen. Within one theme a
//! category always keeps the same colour; a test checks that no two of the first twelve look
//! alike, so neighbouring bars can always be told apart.
//!
//! The palettes live in `core` (plain RGB bytes, no terminal types) so a future Android app
//! can use exactly the same colours. The names and values match tuxwire and zerohour.
//!
//! **Adding a theme = adding ONE entry to [`THEMES`].** The cycling key, the config file and
//! the tests all read that list.

/// A 24-bit colour: red, green, blue, each 0-255.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// Builds a colour from a hex number such as `0x2d353b` (the digits of CSS `#2d353b`),
    /// so palettes can be copied straight from a theme's official page.
    ///
    /// `const fn` lets [`THEMES`] be a compile-time constant. `>>` shifts bits right and
    /// `& 0xff` keeps the lowest 8 bits, which pulls out one channel at a time.
    pub const fn hex(value: u32) -> Rgb {
        Rgb(
            ((value >> 16) & 0xff) as u8,
            ((value >> 8) & 0xff) as u8,
            (value & 0xff) as u8,
        )
    }

    /// The colour a fraction `t` of the way from `self` to `other` (0.0 = self, 1.0 = other).
    /// Used for gradients: the title sparkline and the heat-map cells.
    pub fn lerp(self, other: Rgb, t: f32) -> Rgb {
        let t = t.clamp(0.0, 1.0);
        // Mix one channel: start + (end - start) * t, rounded back into 0..=255.
        let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
        Rgb(
            mix(self.0, other.0),
            mix(self.1, other.1),
            mix(self.2, other.2),
        )
    }
}

/// One complete theme: a name plus a colour for every slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// Display name, also what is saved in `tui.conf`.
    pub name: &'static str,
    /// Background of the whole screen.
    pub bg: Rgb,
    /// Normal text.
    pub fg: Rgb,
    /// Quiet text: key hints, axis labels, empty-state messages.
    pub muted: Rgb,
    /// Box borders and empty bar tracks.
    pub border: Rgb,
    /// Highlights: the selected tab, the title, the start of the sparkline gradient.
    pub accent: Rgb,
    /// The far end of the gradients (the theme's orange, or yellow where the accent is
    /// already orange). Also the colour of the listening line.
    pub banner_end: Rgb,
    /// A faint "fill" colour: empty heat-map cells.
    pub selection: Rgb,
    /// Good / busy: the bright end of the heat map.
    pub ok: Rgb,
    /// Attention (reserved for upcoming warnings).
    pub warn: Rgb,
    /// Problems (reserved for upcoming errors).
    pub danger: Rgb,
    /// The theme's own accent colours, ordered so that neighbours differ the most
    /// (blue, orange, green, purple, ...). They colour the category and application bars.
    /// Fewer than 12 is fine: see [`Theme::category_color`] for how extra slots are made.
    pub accents: &'static [Rgb],
}

/// The default theme (the owner's favourite, as in tuxwire and zerohour).
pub const DEFAULT_THEME: &str = "Everforest";

/// Every built-in theme. The first entry is the default and the cycling order.
pub const THEMES: &[Theme] = &[
    // https://github.com/sainnhe/everforest (dark, medium contrast)
    Theme {
        name: "Everforest",
        bg: Rgb::hex(0x2d353b),
        fg: Rgb::hex(0xd3c6aa),
        muted: Rgb::hex(0x9aa79d),
        border: Rgb::hex(0x56635f),
        accent: Rgb::hex(0xa7c080),
        banner_end: Rgb::hex(0xe69875),
        selection: Rgb::hex(0x3d484d),
        ok: Rgb::hex(0xa7c080),
        warn: Rgb::hex(0xdbbc7f),
        danger: Rgb::hex(0xe67e80),
        accents: &[
            Rgb::hex(0x7fbbb3),
            Rgb::hex(0xe69875),
            Rgb::hex(0xa7c080),
            Rgb::hex(0xd699b6),
            Rgb::hex(0xdbbc7f),
            Rgb::hex(0x83c092),
            Rgb::hex(0xe67e80),
        ],
    },
    // https://draculatheme.com/contribute
    Theme {
        name: "Dracula",
        bg: Rgb::hex(0x282a36),
        fg: Rgb::hex(0xf8f8f2),
        muted: Rgb::hex(0x8a96c4),
        border: Rgb::hex(0x6272a4),
        accent: Rgb::hex(0xbd93f9),
        banner_end: Rgb::hex(0xffb86c),
        selection: Rgb::hex(0x343746),
        ok: Rgb::hex(0x50fa7b),
        warn: Rgb::hex(0xf1fa8c),
        danger: Rgb::hex(0xff5555),
        accents: &[
            Rgb::hex(0x8be9fd),
            Rgb::hex(0xffb86c),
            Rgb::hex(0x50fa7b),
            Rgb::hex(0xbd93f9),
            Rgb::hex(0xf1fa8c),
            Rgb::hex(0xff79c6),
            Rgb::hex(0xff5555),
        ],
    },
    // https://github.com/morhetz/gruvbox (dark)
    Theme {
        name: "Gruvbox",
        bg: Rgb::hex(0x282828),
        fg: Rgb::hex(0xebdbb2),
        muted: Rgb::hex(0xa89984),
        border: Rgb::hex(0x665c54),
        accent: Rgb::hex(0xfe8019),
        banner_end: Rgb::hex(0xfabd2f), // yellow: Gruvbox's accent is already orange
        selection: Rgb::hex(0x3c3836),
        ok: Rgb::hex(0xb8bb26),
        warn: Rgb::hex(0xfabd2f),
        danger: Rgb::hex(0xfb4934),
        accents: &[
            Rgb::hex(0x83a598),
            Rgb::hex(0xfe8019),
            Rgb::hex(0xb8bb26),
            Rgb::hex(0xd3869b),
            Rgb::hex(0xfabd2f),
            Rgb::hex(0x8ec07c),
            Rgb::hex(0xfb4934),
        ],
    },
    // https://catppuccin.com/palette (Mocha)
    Theme {
        name: "Catppuccin Mocha",
        bg: Rgb::hex(0x1e1e2e),
        fg: Rgb::hex(0xcdd6f4),
        muted: Rgb::hex(0x9399b2),
        border: Rgb::hex(0x585b70),
        accent: Rgb::hex(0xcba6f7),
        banner_end: Rgb::hex(0xfab387),
        selection: Rgb::hex(0x313244),
        ok: Rgb::hex(0xa6e3a1),
        warn: Rgb::hex(0xf9e2af),
        danger: Rgb::hex(0xf38ba8),
        accents: &[
            Rgb::hex(0x89b4fa),
            Rgb::hex(0xfab387),
            Rgb::hex(0xa6e3a1),
            Rgb::hex(0xcba6f7),
            Rgb::hex(0xf9e2af),
            Rgb::hex(0x94e2d5),
            Rgb::hex(0xf38ba8),
            Rgb::hex(0xf5c2e7),
        ],
    },
    // https://www.nordtheme.com/docs/colors-and-palettes
    Theme {
        name: "Nord",
        bg: Rgb::hex(0x2e3440),
        fg: Rgb::hex(0xd8dee9),
        muted: Rgb::hex(0xa3aec6),
        border: Rgb::hex(0x4c566a),
        accent: Rgb::hex(0x81a1c1),
        banner_end: Rgb::hex(0xd08770),
        selection: Rgb::hex(0x3b4252),
        ok: Rgb::hex(0xa3be8c),
        warn: Rgb::hex(0xebcb8b),
        danger: Rgb::hex(0xbf616a),
        accents: &[
            Rgb::hex(0x81a1c1),
            Rgb::hex(0xd08770),
            Rgb::hex(0xa3be8c),
            Rgb::hex(0xb48ead),
            Rgb::hex(0xebcb8b),
            Rgb::hex(0x8fbcbb),
            Rgb::hex(0xbf616a),
        ],
    },
    // https://ethanschoonover.com/solarized/ (dark)
    Theme {
        name: "Solarized Dark",
        bg: Rgb::hex(0x002b36),
        fg: Rgb::hex(0x93a1a1),
        muted: Rgb::hex(0x839496),
        border: Rgb::hex(0x586e75),
        accent: Rgb::hex(0x268bd2),
        banner_end: Rgb::hex(0xcb4b16),
        selection: Rgb::hex(0x073642),
        ok: Rgb::hex(0x859900),
        warn: Rgb::hex(0xb58900),
        danger: Rgb::hex(0xdc322f),
        accents: &[
            Rgb::hex(0x268bd2),
            Rgb::hex(0xcb4b16),
            Rgb::hex(0x859900),
            Rgb::hex(0x6c71c4),
            Rgb::hex(0xb58900),
            Rgb::hex(0x2aa198),
            Rgb::hex(0xdc322f),
            Rgb::hex(0xd33682),
        ],
    },
    // https://github.com/folke/tokyonight.nvim (night)
    Theme {
        name: "Tokyo Night",
        bg: Rgb::hex(0x1a1b26),
        fg: Rgb::hex(0xc0caf5),
        muted: Rgb::hex(0x8089b3),
        border: Rgb::hex(0x3b4261),
        accent: Rgb::hex(0x7aa2f7),
        banner_end: Rgb::hex(0xff9e64),
        selection: Rgb::hex(0x24283b),
        ok: Rgb::hex(0x9ece6a),
        warn: Rgb::hex(0xe0af68),
        danger: Rgb::hex(0xf7768e),
        accents: &[
            Rgb::hex(0x7aa2f7),
            Rgb::hex(0xff9e64),
            Rgb::hex(0x9ece6a),
            Rgb::hex(0xbb9af7),
            Rgb::hex(0xe0af68),
            Rgb::hex(0x73daca),
            Rgb::hex(0xf7768e),
            Rgb::hex(0x7dcfff),
        ],
    },
    // https://catppuccin.com/palette (Macchiato, tuxwire's current look)
    Theme {
        name: "Catppuccin Macchiato",
        bg: Rgb::hex(0x24273a),
        fg: Rgb::hex(0xcad3f5),
        muted: Rgb::hex(0x939ab7),
        border: Rgb::hex(0x494d64),
        accent: Rgb::hex(0x8aadf4),
        banner_end: Rgb::hex(0xf5a97f),
        selection: Rgb::hex(0x363a4f),
        ok: Rgb::hex(0xa6da95),
        warn: Rgb::hex(0xeed49f),
        danger: Rgb::hex(0xed8796),
        accents: &[
            Rgb::hex(0x8aadf4),
            Rgb::hex(0xf5a97f),
            Rgb::hex(0xa6da95),
            Rgb::hex(0xc6a0f6),
            Rgb::hex(0xeed49f),
            Rgb::hex(0x8bd5ca),
            Rgb::hex(0xed8796),
            Rgb::hex(0xf5bde6),
        ],
    },
];

impl Theme {
    /// The colour of the category at `index` (its place in the config order); `None` is the
    /// quiet "Other" colour.
    ///
    /// The first slots are the theme's own accents. When a theme has fewer than the 12 an
    /// ordinary setup needs, further slots are made from the same accents: deepened
    /// toward the background or lightened toward white, alternating along the
    /// list so neighbouring hues differ in lightness. A setup with more than three
    /// times the number of accents simply starts the cycle again.
    pub fn category_color(&self, index: Option<usize>) -> Rgb {
        let Some(i) = index else {
            return self.muted.lerp(self.bg, 0.30);
        };
        let base = self.accents.len();
        let (round, k) = ((i / base) % 3, i % base);
        let accent = self.accents[k];
        // Variants alternate deep / light along the accent list, so two variants of
        // neighbouring hues never share a lightness (deep orange next to deep yellow
        // would be hard to tell apart).
        let deep = accent.lerp(self.bg, 0.40);
        let light = accent.lerp(Rgb(255, 255, 255), 0.35);
        match round {
            0 => accent,
            // A deepened colour that would sink into the background is lightened instead.
            _ if (k + round) % 2 == 0 && contrast(deep, self.bg) >= 2.5 => deep,
            _ => light,
        }
    }
}

/// Looks a theme up by name, ignoring upper/lower case. `None` if there is no such theme.
pub fn find(name: &str) -> Option<&'static Theme> {
    THEMES
        .iter()
        .find(|t| t.name.eq_ignore_ascii_case(name.trim()))
}

/// The theme to use for a saved name: the named one, or the default if the name is unknown
/// (for example a theme that was removed in a later version).
pub fn find_or_default(name: &str) -> &'static Theme {
    find(name).unwrap_or(&THEMES[0])
}

/// The theme after `current` in the list, wrapping round to the first.
pub fn next(current: &Theme) -> &'static Theme {
    let at = THEMES
        .iter()
        .position(|t| t.name == current.name)
        .unwrap_or(0);
    &THEMES[(at + 1) % THEMES.len()]
}

/// The relative luminance of a colour (0 = black, 1 = white), as defined by WCAG.
/// Used by the tests to check that text is readable on every background.
pub fn luminance(c: Rgb) -> f64 {
    // Convert one 8-bit channel to linear light.
    let lin = |v: u8| {
        let s = f64::from(v) / 255.0;
        if s <= 0.03928 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.0) + 0.7152 * lin(c.1) + 0.0722 * lin(c.2)
}

/// WCAG contrast ratio between two colours (1 = identical, 21 = black on white).
pub fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_first_and_names_are_unique() {
        assert_eq!(THEMES[0].name, DEFAULT_THEME);
        for (i, a) in THEMES.iter().enumerate() {
            for b in &THEMES[i + 1..] {
                assert_ne!(a.name, b.name, "duplicate theme name");
            }
        }
    }

    #[test]
    fn hex_splits_channels() {
        assert_eq!(Rgb::hex(0x2d353b), Rgb(0x2d, 0x35, 0x3b));
    }

    #[test]
    fn lerp_hits_both_ends_and_the_middle() {
        let (a, b) = (Rgb(0, 0, 0), Rgb(200, 100, 50));
        assert_eq!(a.lerp(b, 0.0), a);
        assert_eq!(a.lerp(b, 1.0), b);
        assert_eq!(a.lerp(b, 0.5), Rgb(100, 50, 25));
        // Out-of-range fractions are clamped.
        assert_eq!(a.lerp(b, 9.0), b);
    }

    #[test]
    fn find_ignores_case_and_unknown_falls_back() {
        assert_eq!(find("  nord ").unwrap().name, "Nord");
        assert!(find("nope").is_none());
        assert_eq!(find_or_default("nope").name, DEFAULT_THEME);
    }

    #[test]
    fn next_cycles_through_all_and_wraps() {
        let mut t = &THEMES[0];
        for _ in 0..THEMES.len() {
            t = next(t);
        }
        assert_eq!(t.name, THEMES[0].name);
    }

    #[test]
    fn text_is_readable_on_every_background() {
        for t in THEMES {
            assert!(contrast(t.fg, t.bg) >= 5.0, "{}: fg on bg", t.name);
            // Muted text carries the key hints, so it must stay comfortably readable.
            assert!(contrast(t.muted, t.bg) >= 4.5, "{}: muted on bg", t.name);
            assert!(contrast(t.accent, t.bg) >= 3.0, "{}: accent on bg", t.name);
            assert!(contrast(t.banner_end, t.bg) >= 3.0, "{}: end on bg", t.name);
            // Borders only need to be visible, not read.
            assert!(contrast(t.border, t.bg) >= 1.5, "{}: border on bg", t.name);
        }
    }

    /// Straight-line distance between two colours in RGB space (0 = identical).
    fn distance(a: Rgb, b: Rgb) -> f64 {
        let d = |x: u8, y: u8| (f64::from(x) - f64::from(y)).powi(2);
        (d(a.0, b.0) + d(a.1, b.1) + d(a.2, b.2)).sqrt()
    }

    #[test]
    fn the_first_twelve_category_colours_are_all_different_in_every_theme() {
        for t in THEMES {
            let colours: Vec<Rgb> = (0..12).map(|i| t.category_color(Some(i))).collect();
            for (i, a) in colours.iter().enumerate() {
                for (j, b) in colours.iter().enumerate().skip(i + 1) {
                    assert!(
                        distance(*a, *b) >= 28.0,
                        "{}: slots {i} and {j} are too alike ({a:?} vs {b:?})",
                        t.name
                    );
                }
                // Each must also stand out from the background, and from the "Other" grey.
                assert!(contrast(*a, t.bg) >= 2.2, "{}: slot {i} on bg", t.name);
                assert!(
                    distance(*a, t.category_color(None)) >= 18.0,
                    "{}: slot {i} looks like Other",
                    t.name
                );
            }
        }
    }

    #[test]
    fn heat_map_ends_are_distinguishable() {
        for t in THEMES {
            assert!(contrast(t.ok, t.selection) >= 3.0, "{}: heat range", t.name);
        }
    }
}
