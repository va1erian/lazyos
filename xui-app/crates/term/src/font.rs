//! Which fonts a terminal can use: reads a font file's family, style and the
//! advances of `i` and `M`, and keeps a catalogue of the monospace families
//! (those whose two advances match: a proportional font cannot hold a
//! character grid). No drawing, no file access: the host reads the bytes.

use std::collections::BTreeMap;

use ttf_parser::{name_id, Face};

/// What one font file declares.
#[derive(Clone, PartialEq, Debug)]
pub struct FaceInfo {
    pub family: String,
    pub bold: bool,
    pub italic: bool,
    /// The advances of `i` and `M` are the same.
    pub monospaced: bool,
    /// The advance of `M` in em (the cell width at size 1).
    pub advance_em: f32,
}

/// Read a font file (the first face of a collection); `None` when it is not
/// a font or lacks a family name or the two probe glyphs.
pub fn parse(bytes: &[u8]) -> Option<FaceInfo> {
    let face = Face::parse(bytes, 0).ok()?;
    let name_of = |id: u16| {
        face.names()
            .into_iter()
            .filter(|name| name.name_id == id)
            .find_map(|name| name.to_string())
    };
    let family = name_of(name_id::TYPOGRAPHIC_FAMILY).or_else(|| name_of(name_id::FAMILY))?;
    let advance = |c: char| face.glyph_hor_advance(face.glyph_index(c)?);
    let (i, m) = (advance('i')?, advance('M')?);
    let em = f32::from(face.units_per_em());
    Some(FaceInfo {
        family,
        bold: face.is_bold() || face.weight().to_number() >= 600,
        italic: face.is_italic() || face.is_oblique(),
        monospaced: i == m && m > 0,
        advance_em: f32::from(m) / em,
    })
}

/// The faces of one family that were seen.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Styles {
    pub regular: bool,
    pub bold: bool,
    pub italic: bool,
    pub bold_italic: bool,
    pub advance_em: f32,
}

/// The monospace families found so far.
#[derive(Default, Debug)]
pub struct Catalog {
    families: BTreeMap<String, Styles>,
}

impl Catalog {
    /// Note one face. A proportional face is ignored, and so is a face whose
    /// advance differs from the family's other faces (a family is offered
    /// only while all its faces agree).
    pub fn add(&mut self, info: &FaceInfo) {
        if !info.monospaced {
            return;
        }
        let styles = self.families.entry(info.family.clone()).or_default();
        if styles.advance_em != 0.0 && (styles.advance_em - info.advance_em).abs() > 1e-3 {
            return;
        }
        styles.advance_em = info.advance_em;
        match (info.bold, info.italic) {
            (false, false) => styles.regular = true,
            (true, false) => styles.bold = true,
            (false, true) => styles.italic = true,
            (true, true) => styles.bold_italic = true,
        }
    }

    /// The offered family names, sorted.
    pub fn names(&self) -> Vec<&str> {
        self.families.keys().map(String::as_str).collect()
    }

    pub fn styles(&self, family: &str) -> Option<Styles> {
        self.families.get(family).copied()
    }

    /// The family to use for a requested name: itself when it is offered
    /// (and has a regular face), else `fallback`.
    pub fn choose<'a>(&'a self, wanted: &'a str, fallback: &'a str) -> &'a str {
        match self.families.get(wanted) {
            Some(styles) if styles.regular => wanted,
            _ => fallback,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JETBRAINS: &[u8] = include_bytes!("../../../../assets/fonts/JetBrainsMono-Regular.ttf");
    const LIBERATION_MONO: &[u8] =
        include_bytes!("../../../../assets/fonts/liberation/LiberationMono-Regular.ttf");
    const LIBERATION_MONO_BOLD: &[u8] =
        include_bytes!("../../../../assets/fonts/liberation/LiberationMono-BoldItalic.ttf");
    const LIBERATION_SANS: &[u8] =
        include_bytes!("../../../../assets/fonts/liberation/LiberationSans-Regular.ttf");
    const DROID_SANS: &[u8] = include_bytes!("../../../../assets/fonts/DroidSans.ttf");

    #[test]
    fn jetbrains_mono_is_monospace_at_six_tenths() {
        let info = parse(JETBRAINS).unwrap();
        assert_eq!(info.family, "JetBrains Mono");
        assert!(info.monospaced && !info.bold && !info.italic);
        assert!((info.advance_em - 0.6).abs() < 1e-3, "{}", info.advance_em);
    }

    #[test]
    fn jetbrains_mono_has_all_four_faces_with_the_regular_advance() {
        let faces: [(&[u8], bool, bool); 4] = [
            (JETBRAINS, false, false),
            (include_bytes!("../../../../assets/fonts/JetBrainsMono-Bold.ttf"), true, false),
            (include_bytes!("../../../../assets/fonts/JetBrainsMono-Italic.ttf"), false, true),
            (include_bytes!("../../../../assets/fonts/JetBrainsMono-BoldItalic.ttf"), true, true),
        ];
        let regular = parse(JETBRAINS).unwrap().advance_em;
        let mut catalog = Catalog::default();
        for (bytes, bold, italic) in faces {
            let info = parse(bytes).unwrap();
            assert_eq!(info.family, "JetBrains Mono");
            assert_eq!((info.bold, info.italic), (bold, italic));
            assert!(info.monospaced, "monospaced");
            assert!((info.advance_em - regular).abs() < 1e-6, "{} vs {regular}", info.advance_em);
            catalog.add(&info);
        }
        let styles = catalog.styles("JetBrains Mono").unwrap();
        assert!(styles.regular && styles.bold && styles.italic && styles.bold_italic);
    }

    #[test]
    fn liberation_mono_reports_its_style() {
        let info = parse(LIBERATION_MONO).unwrap();
        assert_eq!(info.family, "Liberation Mono");
        assert!(info.monospaced);
        let bold_italic = parse(LIBERATION_MONO_BOLD).unwrap();
        assert!(bold_italic.bold && bold_italic.italic);
        assert_eq!(bold_italic.family, "Liberation Mono");
    }

    #[test]
    fn proportional_fonts_are_not_offered() {
        for bytes in [LIBERATION_SANS, DROID_SANS] {
            let info = parse(bytes).unwrap();
            assert!(!info.monospaced, "{}", info.family);
            let mut catalog = Catalog::default();
            catalog.add(&info);
            assert!(catalog.names().is_empty());
        }
    }

    #[test]
    fn garbage_is_not_a_font() {
        assert!(parse(b"").is_none());
        assert!(parse(b"not a font at all, just text").is_none());
        assert!(parse(&JETBRAINS[..200]).is_none());
        let mut broken = JETBRAINS.to_vec();
        broken[..12].fill(0xff);
        assert!(parse(&broken).is_none());
    }

    #[test]
    fn the_catalogue_groups_faces_into_families() {
        let mut catalog = Catalog::default();
        for bytes in [JETBRAINS, LIBERATION_MONO, LIBERATION_MONO_BOLD, LIBERATION_SANS] {
            catalog.add(&parse(bytes).unwrap());
        }
        assert_eq!(catalog.names(), ["JetBrains Mono", "Liberation Mono"]);
        let liberation = catalog.styles("Liberation Mono").unwrap();
        assert!(liberation.regular && liberation.bold_italic && !liberation.bold);
        assert_eq!(catalog.choose("Liberation Mono", "JetBrains Mono"), "Liberation Mono");
        assert_eq!(catalog.choose("Comic Sans", "JetBrains Mono"), "JetBrains Mono");
    }

    #[test]
    fn a_family_without_its_regular_face_is_not_chosen() {
        let mut catalog = Catalog::default();
        catalog.add(&parse(LIBERATION_MONO_BOLD).unwrap());
        assert_eq!(catalog.names(), ["Liberation Mono"]);
        assert_eq!(catalog.choose("Liberation Mono", "JetBrains Mono"), "JetBrains Mono");
    }

    #[test]
    fn a_face_that_disagrees_with_its_family_is_dropped() {
        let mut catalog = Catalog::default();
        let mut wide = parse(LIBERATION_MONO).unwrap();
        catalog.add(&wide);
        wide.bold = true;
        wide.advance_em += 0.1;
        catalog.add(&wide);
        assert!(!catalog.styles("Liberation Mono").unwrap().bold);
    }
}
