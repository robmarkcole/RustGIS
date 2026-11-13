//! The document's fonts: each face text is set in, embedded as itself where it can be (a subset
//! of its file, as a Type 0 font: [`super::embed`]), else as Type 3 fonts whose glyphs are the
//! face's outlines, in font units, 256 glyphs to a font (a Type 3 font's codes are single bytes);
//! each font with the map from its codes back to the characters they stand for.

use std::collections::HashMap;
use std::fmt::Write as _;

use ab_glyph::{Font, FontRef, GlyphId, OutlineCurve};

use super::Doc;
use super::content::num;
use super::embed::Embedded;

#[derive(Default)]
pub(super) struct Fonts {
    faces: Vec<Face>,
    /// Faces by the address of their `FontRef` (fonts are parsed once and kept).
    by_font: HashMap<usize, usize>,
    /// The faces embedded as themselves, in the order given (so a document is written the same
    /// way each time), and their places there by the same address.
    real: Vec<Real>,
    real_by_font: HashMap<usize, usize>,
}

/// A face embedded as itself: its Type 0 font's object number, its subset, and the glyphs set
/// in it by their CIDs.
struct Real {
    id: usize,
    font: &'static FontRef<'static>,
    embedded: Embedded,
    glyphs: std::collections::BTreeMap<u16, char>,
}

struct Face {
    font: &'static FontRef<'static>,
    upm: f32,
    chunks: Vec<Chunk>,
    /// Each glyph's chunk and code.
    codes: HashMap<u16, (usize, u8)>,
}

struct Chunk {
    /// The font's object number.
    id: usize,
    glyphs: Vec<(GlyphId, char)>,
}

/// Where a glyph is: its font's object number, its code there (two bytes, a CID, when `wide`)
/// and its advance in ems.
pub(super) struct Placed {
    pub font: usize,
    pub code: u16,
    pub wide: bool,
    pub advance: f32,
}

/// A face's advance for `id` in ems, 0 where it isn't a number.
fn advance_em(font: &FontRef<'static>, upm: f32, id: GlyphId) -> f32 {
    let advance = font.h_advance_unscaled(id) / upm;
    if advance.is_finite() { advance } else { 0.0 }
}

/// A face's units per em, read as the usual 1000 where it has none usable.
fn upm_of(font: &FontRef<'static>) -> f32 {
    font.units_per_em().filter(|u| u.is_finite() && *u > 0.0).unwrap_or(1000.0)
}

impl Fonts {
    /// Fonts that set the faces of `real` as themselves, each given its font's number now.
    pub fn with_real(doc: &mut Doc, real: Vec<(&'static FontRef<'static>, Embedded)>) -> Fonts {
        let real_by_font = real.iter().enumerate().map(|(i, (font, _))| (std::ptr::from_ref(*font) as usize, i)).collect();
        let real =
            real.into_iter().map(|(font, embedded)| Real { id: doc.reserve(), font, embedded, glyphs: Default::default() }).collect();
        Fonts { real, real_by_font, ..Fonts::default() }
    }

    /// Whether `font` is embedded as itself (else its glyphs go to Type 3 fonts).
    pub fn is_real(&self, font: &FontRef<'static>) -> bool {
        self.real_by_font.contains_key(&(std::ptr::from_ref(font) as usize))
    }

    /// `id` of `font` (standing for `ch`), given a code in one of its Type 3 fonts the first time;
    /// `None` only past what a font can hold.
    pub fn place(&mut self, doc: &mut Doc, font: &'static FontRef<'static>, id: GlyphId, ch: char) -> Option<Placed> {
        let key = std::ptr::from_ref(font) as usize;
        // An embedded face's glyph is its CID in the subset (every glyph the document sets was
        // given one before the content was written).
        if let Some(r) = self.real_by_font.get(&key).and_then(|&i| self.real.get_mut(i))
            && let Some(cid) = r.embedded.remapper.get(id.0)
        {
            r.glyphs.entry(cid).or_insert(ch);
            return Some(Placed { font: r.id, code: cid, wide: true, advance: advance_em(font, upm_of(font), id) });
        }
        let fi = match self.by_font.get(&key) {
            Some(&i) => i,
            None => {
                // A face with no usable units per em is read as the usual 1000, as the pixels read
                // it at one unit a pixel's em: its text is kept, not left out.
                let upm = upm_of(font);
                self.faces.push(Face { font, upm, chunks: Vec::new(), codes: HashMap::new() });
                self.by_font.insert(key, self.faces.len() - 1);
                self.faces.len() - 1
            }
        };
        let face = self.faces.get_mut(fi)?;
        let advance = advance_em(font, face.upm, id);
        let (ci, code) = match face.codes.get(&id.0) {
            Some(&c) => c,
            None => {
                if face.chunks.last().is_none_or(|c| c.glyphs.len() >= 256) {
                    face.chunks.push(Chunk { id: doc.reserve(), glyphs: Vec::new() });
                }
                let ci = face.chunks.len() - 1;
                let chunk = face.chunks.get_mut(ci)?;
                let code = u8::try_from(chunk.glyphs.len()).ok()?;
                chunk.glyphs.push((id, ch));
                face.codes.insert(id.0, (ci, code));
                (ci, code)
            }
        };
        Some(Placed { font: face.chunks.get(ci)?.id, code: u16::from(code), wide: false, advance })
    }

    /// Writes every font given a glyph.
    pub fn write(self, doc: &mut Doc) {
        for r in &self.real {
            write_real(doc, r);
        }
        for face in &self.faces {
            for chunk in &face.chunks {
                write_chunk(doc, face, chunk);
            }
        }
    }
}

/// One Type 3 font: its glyph procedures, widths, encoding and map back to Unicode.
fn write_chunk(doc: &mut Doc, face: &Face, chunk: &Chunk) {
    let mut procs = String::new();
    let mut widths = String::new();
    let mut names = String::new();
    let mut bbox = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for (code, (id, _)) in chunk.glyphs.iter().enumerate() {
        let advance = face.font.h_advance_unscaled(*id);
        let advance = if advance.is_finite() { advance } else { 0.0 };
        let (ops, b) = glyph_procedure(face.font, *id, advance);
        if let Some(b) = b {
            bbox = [bbox[0].min(b[0]), bbox[1].min(b[1]), bbox[2].max(b[2]), bbox[3].max(b[3])];
        }
        let proc_id = doc.reserve();
        doc.stream(proc_id, "", &ops, false);
        let _ = write!(procs, "/g{code} {proc_id} 0 R ");
        let _ = write!(names, "/g{code}");
        let mut w = Vec::new();
        num(&mut w, advance, 2);
        widths.push_str(&String::from_utf8_lossy(&w));
        widths.push(' ');
    }
    if bbox[0] > bbox[2] {
        bbox = [0.0; 4];
    }
    let to_unicode = doc.reserve();
    let codes: Vec<(u16, char)> = chunk.glyphs.iter().zip(0u16..).map(|((_, ch), code)| (code, *ch)).collect();
    doc.stream(to_unicode, "", to_unicode_cmap(&codes, false).as_bytes(), false);
    let mut matrix = Vec::new();
    for v in [1.0 / face.upm, 0.0, 0.0, 1.0 / face.upm, 0.0, 0.0] {
        num(&mut matrix, v, 9);
        matrix.push(b' ');
    }
    let mut b = Vec::new();
    for v in bbox {
        num(&mut b, v, 0);
        b.push(b' ');
    }
    doc.put(
        chunk.id,
        format!(
            "<< /Type /Font /Subtype /Type3 /FontBBox [{}] /FontMatrix [{}] /CharProcs << {procs}>> /Encoding << /Type /Encoding /Differences [0 {names}] >> /FirstChar 0 /LastChar {} /Widths [{widths}] /Resources << >> /ToUnicode {to_unicode} 0 R >>",
            String::from_utf8_lossy(&b).trim_end(),
            String::from_utf8_lossy(&matrix).trim_end(),
            chunk.glyphs.len().saturating_sub(1),
        )
        .as_bytes(),
    );
}

/// An embedded face's fonts: the Type 0 font over its CID font, the CID font's descriptor and
/// file, its glyphs' widths and the map from its CIDs back to their characters.
fn write_real(doc: &mut Doc, r: &Real) {
    let e = &r.embedded;
    let (cid_font, descriptor, file, to_unicode) = (doc.reserve(), doc.reserve(), doc.reserve(), doc.reserve());
    let upm = upm_of(r.font);
    let gids: Vec<u16> = e.remapper.remapped_gids().collect();
    let mut widths = String::new();
    for &cid in r.glyphs.keys() {
        let gid = gids.get(usize::from(cid)).copied().unwrap_or(0);
        let mut w = Vec::new();
        num(&mut w, advance_em(r.font, upm, GlyphId(gid)) * 1000.0, 2);
        let _ = write!(widths, "{cid} [{}] ", String::from_utf8_lossy(&w));
    }
    let n = |v: f32| {
        let mut b = Vec::new();
        num(&mut b, if v.is_finite() { v } else { 0.0 }, 1);
        String::from_utf8_lossy(&b).into_owned()
    };
    // The file: TrueType outlines (FontFile2), or CFF ones in an OpenType wrapper (FontFile3).
    let (subtype, file_key, file_dict, gid_map) = if e.cff {
        ("CIDFontType0", "FontFile3", "/Subtype /OpenType".to_string(), "")
    } else {
        ("CIDFontType2", "FontFile2", format!("/Length1 {}", e.file.len()), " /CIDToGIDMap /Identity")
    };
    doc.stream(file, &file_dict, &e.file, false);
    // Symbolic (its glyphs are reached by CID, not a standard encoding), and italic where it leans.
    let flags = 4 + if e.italic_angle.abs() > 0.01 { 64 } else { 0 };
    let [x0, y0, x1, y1] = e.bbox;
    doc.put(
        descriptor,
        format!(
            "<< /Type /FontDescriptor /FontName /{} /Flags {flags} /FontBBox [{} {} {} {}] /ItalicAngle {} /Ascent {} /Descent {} /CapHeight {} /StemV 80 /{file_key} {file} 0 R >>",
            e.name,
            n(x0),
            n(y0),
            n(x1),
            n(y1),
            n(e.italic_angle),
            n(e.ascent),
            n(e.descent),
            n(e.cap_height)
        )
        .as_bytes(),
    );
    doc.put(
        cid_font,
        format!(
            "<< /Type /Font /Subtype /{subtype} /BaseFont /{} /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor {descriptor} 0 R /DW 0 /W [{widths}]{gid_map} >>",
            e.name
        )
        .as_bytes(),
    );
    let codes: Vec<(u16, char)> = r.glyphs.iter().map(|(c, ch)| (*c, *ch)).collect();
    doc.stream(to_unicode, "", to_unicode_cmap(&codes, true).as_bytes(), false);
    doc.put(
        r.id,
        format!(
            "<< /Type /Font /Subtype /Type0 /BaseFont /{} /Encoding /Identity-H /DescendantFonts [{cid_font} 0 R] /ToUnicode {to_unicode} 0 R >>",
            e.name
        )
        .as_bytes(),
    );
}

/// A glyph's procedure: its advance and box (`d1`, so it takes the text's colour) and its
/// outline, filled; and its box in font units, `None` for a glyph with no outline (a space).
fn glyph_procedure(font: &FontRef<'static>, id: GlyphId, advance: f32) -> (Vec<u8>, Option<[f32; 4]>) {
    let mut path = Vec::new();
    let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    let mut grow = |p: ab_glyph::Point| {
        if p.x.is_finite() && p.y.is_finite() {
            b = [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)];
        }
    };
    let pt = |out: &mut Vec<u8>, p: ab_glyph::Point| {
        num(out, p.x, 1);
        out.push(b' ');
        num(out, p.y, 1);
        out.push(b' ');
    };
    if let Some(o) = font.outline(id) {
        let mut last: Option<ab_glyph::Point> = None;
        // Every curve: the outline is already read, and a glyph cut short would draw wrong.
        for c in &o.curves {
            let (start, end) = match c {
                OutlineCurve::Line(a, b) | OutlineCurve::Quad(a, _, b) | OutlineCurve::Cubic(a, _, _, b) => (*a, *b),
            };
            if last != Some(start) {
                if last.is_some() {
                    path.extend_from_slice(b"h\n");
                }
                grow(start);
                pt(&mut path, start);
                path.extend_from_slice(b"m\n");
            }
            match c {
                OutlineCurve::Line(_, e) => {
                    grow(*e);
                    pt(&mut path, *e);
                    path.extend_from_slice(b"l\n");
                }
                OutlineCurve::Quad(s, k, e) => {
                    // A quadratic as the cubic with the same curve.
                    let lerp = |a: ab_glyph::Point, t: f32| ab_glyph::point(a.x + (k.x - a.x) * t, a.y + (k.y - a.y) * t);
                    for p in [lerp(*s, 2.0 / 3.0), lerp(*e, 2.0 / 3.0), *e] {
                        grow(p);
                        pt(&mut path, p);
                    }
                    path.extend_from_slice(b"c\n");
                }
                OutlineCurve::Cubic(_, k1, k2, e) => {
                    for p in [*k1, *k2, *e] {
                        grow(p);
                        pt(&mut path, p);
                    }
                    path.extend_from_slice(b"c\n");
                }
            }
            last = Some(end);
        }
        if last.is_some() {
            path.extend_from_slice(b"h\nf\n");
        }
    }
    let bbox = (b[0] <= b[2]).then_some(b);
    let mut ops = Vec::with_capacity(path.len() + 48);
    num(&mut ops, advance, 2);
    ops.extend_from_slice(b" 0 ");
    let [x0, y0, x1, y1] = bbox.unwrap_or([0.0; 4]);
    for v in [x0.floor(), y0.floor(), x1.ceil(), y1.ceil()] {
        num(&mut ops, v, 0);
        ops.push(b' ');
    }
    ops.extend_from_slice(b"d1\n");
    ops.extend_from_slice(&path);
    (ops, bbox)
}

/// The CMap taking each code of a font (one byte, or two when `wide`) to the character its glyph
/// stands for.
fn to_unicode_cmap(codes: &[(u16, char)], wide: bool) -> String {
    let space = if wide { "<0000> <FFFF>" } else { "<00> <FF>" };
    let mut s = format!(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n{space}\nendcodespacerange\n",
    );
    // At most 100 entries to a block.
    for block in codes.chunks(100) {
        let _ = writeln!(s, "{} beginbfchar", block.len());
        for (code, ch) in block {
            let hex: String = ch.encode_utf16(&mut [0; 2]).iter().map(|u| format!("{u:04X}")).collect();
            let _ = if wide { writeln!(s, "<{code:04X}> <{hex}>") } else { writeln!(s, "<{code:02X}> <{hex}>") };
        }
        s.push_str("endbfchar\n");
    }
    s.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyphs_share_a_font_until_it_holds_256() {
        let font = crate::text::label_font().unwrap();
        let mut doc = Doc::new();
        let mut fonts = Fonts::default();
        let a = fonts.place(&mut doc, font, font.glyph_id('A'), 'A').unwrap();
        let again = fonts.place(&mut doc, font, font.glyph_id('A'), 'A').unwrap();
        assert_eq!((a.font, a.code), (again.font, again.code));
        assert!(a.advance > 0.3 && a.advance < 1.0, "an em fraction: {}", a.advance);
        // Glyph ids past what one font holds go to a second.
        let mut last = a;
        for g in 1..300u16 {
            last = fonts.place(&mut doc, font, GlyphId(g), 'x').unwrap();
        }
        assert_ne!(last.font, again.font);
        // 'A' itself may be one of those ids, placed once.
        let distinct = 299 + usize::from(!(1..300).contains(&font.glyph_id('A').0));
        assert_eq!(fonts.faces[0].chunks.iter().map(|c| c.glyphs.len()).collect::<Vec<_>>(), vec![256, distinct - 256]);
    }

    #[test]
    fn a_glyph_procedure_is_its_outline_in_font_units() {
        let font = crate::text::label_font().unwrap();
        let id = font.glyph_id('H');
        let (ops, bbox) = glyph_procedure(font, id, font.h_advance_unscaled(id));
        let ops = String::from_utf8(ops).unwrap();
        assert!(ops.contains(" d1\n") && ops.contains(" m\n") && ops.contains(" l\n") && ops.trim_end().ends_with("h\nf"), "{ops}");
        let [x0, y0, x1, y1] = bbox.unwrap();
        // An H stands on the baseline, a cap height tall.
        let upm = font.units_per_em().unwrap();
        assert!(y0.abs() < upm * 0.02 && (y1 - y0) > upm * 0.5 && x1 > x0, "{bbox:?}");
        let (space, none) = glyph_procedure(font, font.glyph_id(' '), 200.0);
        assert!(none.is_none());
        assert_eq!(String::from_utf8(space).unwrap(), "200 0 0 0 0 0 d1\n");
    }

    #[test]
    fn the_unicode_map_spans_blocks_and_astral_characters() {
        let glyphs: Vec<(u16, char)> = (0..150).map(|i| (i, if i == 120 { '🙂' } else { 'é' })).collect();
        let cmap = to_unicode_cmap(&glyphs, false);
        assert_eq!(cmap.matches("beginbfchar").count(), 2);
        assert!(cmap.contains("100 beginbfchar") && cmap.contains("50 beginbfchar"));
        assert!(cmap.contains("<00> <00E9>") && cmap.contains("<78> <D83DDE42>"));
    }
}
