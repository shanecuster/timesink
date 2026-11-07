//! Category colours, shared by the terminal screens and the HTML export.

/// Colours given out to categories, in the order they appear in `apps.conf`.
/// They are spread around the colour wheel so neighbours are easy to tell apart.
pub const PALETTE_RGB: [(u8, u8, u8); 12] = [
    (86, 156, 214),  // blue
    (230, 145, 80),  // orange
    (140, 200, 100), // green
    (181, 137, 214), // purple
    (229, 192, 70),  // yellow
    (64, 200, 180),  // teal
    (224, 95, 105),  // red
    (235, 130, 190), // pink
    (120, 120, 230), // indigo
    (190, 150, 110), // tan
    (170, 70, 120),  // plum
    (150, 165, 80),  // olive
];

/// Colour for "Other" and anything not named in the config.
pub const OTHER_RGB: (u8, u8, u8) = (128, 128, 128);

/// The colour of the category at `index` in the config order, or the grey
/// "Other" colour when it is not in the list.
///
/// Past the end of the palette the colours are reused but darker, so a repeat
/// is never identical to its first use.
pub fn rgb_for(index: Option<usize>) -> (u8, u8, u8) {
    let Some(i) = index else { return OTHER_RGB };
    let (r, g, b) = PALETTE_RGB[i % PALETTE_RGB.len()];
    if i >= PALETTE_RGB.len() {
        let dim = |c: u8| (c as u16 * 65 / 100) as u8;
        (dim(r), dim(g), dim(b))
    } else {
        (r, g, b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_is_grey_and_repeats_are_darker() {
        assert_eq!(rgb_for(None), OTHER_RGB);
        assert_eq!(rgb_for(Some(0)), PALETTE_RGB[0]);
        assert_ne!(rgb_for(Some(12)), rgb_for(Some(0)));
        let (r0, _, _) = rgb_for(Some(0));
        let (r12, _, _) = rgb_for(Some(12));
        assert!(r12 < r0);
    }
}
