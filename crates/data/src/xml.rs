//! XML parsed safely: every reader of XML (GPX, KML, GML, CityGML, GeoRSS, LandXML, OGC
//! capabilities, raster PAM files, S3 listings, QGIS projects and styles) goes through [`parse`], which refuses a
//! document nested deeper than [`MAX_DEPTH`], or whose namespaces would take more than
//! [`NAMESPACE_WORK`] times its size to resolve, before parsing it. Nesting thousands of elements
//! deep exhausts the stack while the document is parsed or walked, and crafted namespace
//! declarations (or attributes) make the parse grow as their number times the elements they stand
//! around; a hostile file must be an error, never a crash. Where a DTD is allowed, a document whose
//! entities expand to far more than it is long is refused too.

use std::collections::HashMap;

use roxmltree::{Document, ParsingOptions};

/// How deep elements may nest. Real files stay far below it (KML folders, GML geometry and WMS
/// layer trees are a few dozen deep at most).
pub const MAX_DEPTH: usize = 256;

/// How many steps of namespace resolution a document may take per byte of its text. The parser
/// (roxmltree) looks each element's prefix and each attribute's up among the namespaces in scope one
/// by one, checks each attribute against the element's others, and, for an element that declares
/// namespaces, checks each declaration against the others and copies each namespace in scope in,
/// checking it against all it holds so far. A real file (a few dozen namespaces on its root, an
/// element every few dozen bytes) takes under one step a byte; one declaring hundreds of namespaces
/// around many elements, thousands. An element declaring namespaces costs about half the square of
/// those around it, so tiny ones (20 bytes) each declaring one under about 50 namespaces reach it.
/// Each namespace copied in stays in the parsed document (two bytes), and copying n takes n(n-1)/2
/// steps, so within this bound the copies come to about three a byte at most.
pub const NAMESPACE_WORK: u64 = 64;

/// The namespace resolution any document may take, however short: a root declaring several hundred
/// namespaces is no threat.
const MIN_NAMESPACE_WORK: u64 = 1 << 20;

/// [`check_limits`]'s refusal for namespaces that take too much to resolve.
pub const TOO_MANY_NAMESPACES: &str = "the document's namespace declarations would take more to resolve than its size allows";

/// What the entities used in a document may cost ([`expansion`]: bytes of text and declarations
/// searched), per byte of the document: the parser bounds references within an entity, not how
/// often the document uses it.
const ENTITY_EXPANSION: u64 = 16;

/// What any document's entities may expand to, however short.
const MIN_ENTITY_EXPANSION: u64 = 1 << 20;

/// How deep the parser follows an entity's references to others.
const ENTITY_DEPTH: usize = 10;

/// The bytes of an entity's name that count as one step of that cost: comparing or skipping a name
/// is far quicker than expanding text.
const NAME_BYTES_PER_STEP: u64 = 16;

/// [`check_limits`]'s refusal for entities that expand to too much text.
pub const TOO_MUCH_ENTITY_TEXT: &str = "the document's entities would expand to more than its size allows";

/// How [`check_limits`]'s refusal for elements nested past [`MAX_DEPTH`] starts.
pub const TOO_DEEP: &str = "the document nests elements more than";

/// Parses `text` (a leading byte-order mark dropped) with `opts`, once [`check_limits`] passes.
pub fn parse<'a>(text: &'a str, opts: ParsingOptions<'a>) -> Result<Document<'a>, String> {
    let mut unbounded = u64::MAX;
    parse_within(text, opts, &mut unbounded)
}

/// Parses `text` as [`parse`] does, and takes the namespace work [`check_limits`] counts from
/// `left`, refusing it when more than is left. A reader that parses many documents cut from one
/// file gives them [`namespace_budget`] of that file: each document's own bound covers a short one
/// whatever it declares, so documents that each repeat the same declarations (CityGML's members,
/// each with its root's) would otherwise take as their number times the declarations' square.
pub fn parse_within<'a>(text: &'a str, opts: ParsingOptions<'a>, left: &mut u64) -> Result<Document<'a>, String> {
    let text = text.trim_start_matches('\u{feff}');
    let work = check_limits(text, opts.allow_dtd)?;
    *left = left.checked_sub(work).ok_or_else(too_much)?;
    Document::parse_with_options(text, opts).map_err(|e| e.to_string())
}

/// The namespace work a document of `len` bytes may take: [`NAMESPACE_WORK`] steps a byte, and
/// [`MIN_NAMESPACE_WORK`] at least.
pub fn namespace_budget(len: usize) -> u64 {
    (len as u64).saturating_mul(NAMESPACE_WORK).max(MIN_NAMESPACE_WORK)
}

fn too_much() -> String {
    TOO_MANY_NAMESPACES.to_string()
}

/// Refuses XML nested deeper than [`MAX_DEPTH`], or whose namespaces would take more than
/// [`NAMESPACE_WORK`] steps a byte to resolve, by a scan that never recurses. It counts what opens and closes an element, skipping comments,
/// CDATA, processing instructions and quoted attribute values, and the prefixes in scope as the
/// parser does (one declared again is the same prefix); where it can't tell, it counts too deep
/// rather than too shallow, and too much work rather than too little (an attribute with no prefix
/// counts as looked up). With a DTD allowed, an entity whose value holds markup is refused too:
/// expanded, it would nest in ways the text doesn't show. A document is refused as well when the
/// uses of its entities after the DOCTYPE (in comments and CDATA too) expand to more than both
/// [`ENTITY_EXPANSION`] bytes per byte of it and [`MIN_ENTITY_EXPANSION`].
/// Returns the steps of namespace work it counted.
pub fn check_limits(text: &str, allow_dtd: bool) -> Result<u64, String> {
    let b = text.as_bytes();
    let (mut i, mut depth) = (0usize, 0usize);
    // The prefixes each open element declares, how many declarations of each prefix are open (the
    // prefixes in scope), and the steps resolving them takes so far, up to `limit`; in 64 bits, so
    // the browser's 32-bit sizes can't saturate the limit away.
    let mut declared: Vec<Vec<&[u8]>> = Vec::new();
    let mut prefixes: HashMap<&[u8], usize> = HashMap::new();
    let mut work = 0u64;
    let mut counted = false;
    let limit = namespace_budget(text.len());
    let find = |from: usize, pat: &[u8]| -> Option<usize> { b.get(from..)?.windows(pat.len()).position(|w| w == pat).map(|p| from + p) };
    while i < b.len() {
        if b.get(i) != Some(&b'<') {
            i += 1;
            continue;
        }
        let rest = b.get(i..).unwrap_or(&[]);
        if rest.starts_with(b"<!--") {
            match find(i + 4, b"-->") {
                Some(e) => i = e + 3,
                None => return Ok(work),
            }
        } else if rest.starts_with(b"<![CDATA[") {
            match find(i + 9, b"]]>") {
                Some(e) => i = e + 3,
                None => return Ok(work),
            }
        } else if rest.starts_with(b"<?") {
            match find(i + 2, b"?>") {
                Some(e) => i = e + 2,
                None => return Ok(work),
            }
        } else if rest.starts_with(b"<!") {
            // A DOCTYPE (with any internal subset): to its closing `>`, `[ … ]` and quotes skipped.
            // Comments and processing instructions in the subset are skipped whole (an apostrophe in
            // one isn't a quote). A declaration that never ends is refused: the rest of the
            // document would go uncounted.
            let mut j = i + 2;
            let (mut bracket, mut quote) = (0usize, None::<u8>);
            let mut closed = false;
            let mut entities: Vec<(&[u8], &[u8])> = Vec::new();
            while let Some(&c) = b.get(j) {
                if quote.is_none() && b.get(j..).is_some_and(|r| r.starts_with(b"<!--")) {
                    j = find(j + 4, b"-->").map(|e| e + 3).ok_or("the document's DOCTYPE has an unclosed comment")?;
                    continue;
                }
                if quote.is_none() && b.get(j..).is_some_and(|r| r.starts_with(b"<?")) {
                    j = find(j + 2, b"?>").map(|e| e + 2).ok_or("the document's DOCTYPE has an unclosed instruction")?;
                    continue;
                }
                if quote.is_none() && bracket > 0 {
                    let at = b.get(j..).unwrap_or(&[]);
                    // The parser ends these at their first `>`, in quotes or not: so does this, to
                    // read what follows as it does.
                    if [&b"<!ELEMENT"[..], b"<!ATTLIST", b"<!NOTATION"].iter().any(|d| at.starts_with(d)) {
                        j = find(j, b">").map(|e| e + 1).ok_or("the document's DOCTYPE never ends")?;
                        continue;
                    }
                    if allow_dtd {
                        entities.extend(entity(at));
                    }
                }
                match (quote, c) {
                    (Some(q), c) if c == q => quote = None,
                    (Some(_), _) => {}
                    (None, b'"' | b'\'') => quote = Some(c),
                    (None, b'[') => bracket += 1,
                    (None, b']') => bracket = bracket.saturating_sub(1),
                    (None, b'>') if bracket == 0 => {
                        closed = true;
                        break;
                    }
                    _ => {}
                }
                j += 1;
            }
            if !closed {
                return Err("the document's DOCTYPE never ends".into());
            }
            let decl = b.get(i..j).unwrap_or(&[]);
            if allow_dtd && entity_with_markup(decl) {
                return Err("the document declares entities holding markup, which aren't read".into());
            }
            let most = (text.len() as u64).saturating_mul(ENTITY_EXPANSION).max(MIN_ENTITY_EXPANSION);
            // Once: a document has one DOCTYPE, and each count reads all that follows.
            if !entities.is_empty() && !std::mem::replace(&mut counted, true) && expansion(&entities, b.get(j..).unwrap_or(&[])) > most {
                return Err(TOO_MUCH_ENTITY_TEXT.into());
            }
            i = j + 1;
        } else if rest.starts_with(b"</") {
            depth = depth.saturating_sub(1);
            // Its name is compared with its start tag's; the prefixes its element declared go out of
            // scope.
            for prefix in declared.pop().unwrap_or_default() {
                close(&mut prefixes, prefix);
            }
            i = find(i + 2, b">").map(|e| e + 1).unwrap_or(b.len());
        } else {
            // An element's start tag: to its `>` (quoted values skipped); `/>` closes it at once.
            depth += 1;
            if depth > MAX_DEPTH {
                return Err(format!("{TOO_DEEP} {MAX_DEPTH} deep"));
            }
            let mut j = i + 1;
            let mut quote = None::<u8>;
            // Its attributes, by the name before each `=`: the prefixes it declares (`""` the
            // default namespace, which any attribute named `xmlns` declares, `n:xmlns` too, as
            // the parser reads it) and the others.
            let (mut own, mut attributes) = (Vec::<&[u8]>::new(), 0u64);
            let (mut word, mut name) = (None::<usize>, (0usize, 0usize));
            while let Some(&c) = b.get(j) {
                match (quote, c) {
                    (Some(q), c) if c == q => quote = None,
                    (Some(_), _) => {}
                    (None, b'"' | b'\'') => quote = Some(c),
                    (None, b'>') => break,
                    (None, b'=') => {
                        if let Some(s) = word.take() {
                            name = (s, j);
                        }
                        let key = b.get(name.0..name.1).unwrap_or(&[]);
                        if let Some(prefix) = key.strip_prefix(b"xmlns:") {
                            own.push(prefix);
                        } else if key == b"xmlns" || key.ends_with(b":xmlns") {
                            own.push(b"");
                        } else {
                            attributes += 1;
                        }
                    }
                    (None, c) if c.is_ascii_whitespace() || c == b'/' => {
                        if let Some(s) = word.take() {
                            name = (s, j);
                        }
                    }
                    (None, _) => {
                        word.get_or_insert(j);
                    }
                }
                j += 1;
            }
            // The namespaces in scope around it and in it, the `xml` one among them.
            let around = prefixes.len() as u64 + 1;
            for prefix in &own {
                *prefixes.entry(prefix).or_default() += 1;
            }
            let scope = prefixes.len() as u64 + 1;
            let mine = own.len() as u64;
            // Its name and each attribute's prefix are looked up among the namespaces in scope, and
            // each attribute is checked against the others.
            let mut cost = scope.saturating_mul(attributes.saturating_add(1)).saturating_add(attributes.saturating_mul(attributes));
            if mine > 0 {
                // Each declaration is checked against the others; each namespace around it is copied
                // in, checked against its own and those copied before it.
                let copying = around.saturating_mul(mine).saturating_add(around.saturating_mul(around.saturating_sub(1)) / 2);
                cost = cost.saturating_add(mine.saturating_mul(mine)).saturating_add(copying);
            }
            work = work.saturating_add(cost);
            if work > limit {
                return Err(too_much());
            }
            if j > 0 && b.get(j - 1) == Some(&b'/') {
                depth = depth.saturating_sub(1);
                for prefix in own {
                    close(&mut prefixes, prefix);
                }
            } else {
                declared.push(own);
            }
            i = j + 1;
        }
    }
    Ok(work)
}

/// One declaration of `prefix` goes out of scope.
fn close(prefixes: &mut HashMap<&[u8], usize>, prefix: &[u8]) {
    if let Some(open) = prefixes.get_mut(prefix) {
        *open = open.saturating_sub(1);
        if *open == 0 {
            prefixes.remove(prefix);
        }
    }
}

/// The name and quoted value of the entity `decl` starts by declaring, if it does. A parameter
/// entity (`%`) counts too: roxmltree expands it as a general one.
fn entity(decl: &[u8]) -> Option<(&[u8], &[u8])> {
    let rest = decl.strip_prefix(b"<!ENTITY")?;
    if !rest.first()?.is_ascii_whitespace() {
        return None;
    }
    let rest = rest.trim_ascii_start();
    let rest = rest.strip_prefix(b"%").map_or(rest, |r| r.trim_ascii_start());
    // A name ends before the next declaration at the latest, so reading them all stays linear.
    let end = rest.iter().position(|&c| c.is_ascii_whitespace() || matches!(c, b'"' | b'\'' | b'<'))?;
    let (name, rest) = rest.split_at(end);
    let (&quote, rest) = rest.trim_ascii_start().split_first()?;
    // An external entity's keyword stands where the quote would.
    if !matches!(quote, b'"' | b'\'') {
        return None;
    }
    Some((name, rest.get(..rest.iter().position(|&c| c == quote)?)?))
}

/// The names `text` refers to as `&name;`.
fn references(text: &[u8]) -> impl Iterator<Item = &[u8]> {
    text.split(|&c| c == b'&').skip(1).filter_map(|after| after.get(..after.iter().position(|&c| c == b';')?))
}

/// What the references of `body` to `entities` cost the parser: the bytes each expands to, and
/// for each reference, those within values too, the search it makes among the declarations, one by
/// one, and the name it reads (a step each declaration, and each [`NAME_BYTES_PER_STEP`] of a
/// name). Each entity's own references are followed [`ENTITY_DEPTH`] deep; of an entity declared
/// twice, the longer.
fn expansion(entities: &[(&[u8], &[u8])], body: &[u8]) -> u64 {
    let mut costs: HashMap<&[u8], u64> = HashMap::new();
    let steps = |name: &[u8]| name.len() as u64 / NAME_BYTES_PER_STEP;
    let search = entities.iter().fold(0u64, |sum, &(name, _)| sum.saturating_add(1 + steps(name)));
    // What `text` comes to: its own bytes, a known reference's `&name;` giving way to its cost.
    let cost = |costs: &HashMap<&[u8], u64>, text: &[u8]| {
        references(text).filter_map(|name| Some((name, costs.get(name)?))).fold((0u64, 0u64), |(written, sum), (name, &n)| {
            (written + name.len() as u64 + 2, sum.saturating_add(n).saturating_add(search).saturating_add(steps(name)))
        })
    };
    // One pass a level: what each value came to with its references at the level before.
    for _ in 0..=ENTITY_DEPTH {
        let mut next: HashMap<&[u8], u64> = HashMap::new();
        for &(name, value) in entities {
            let (written, sum) = cost(&costs, value);
            let came = (value.len() as u64).saturating_sub(written).saturating_add(sum);
            let longest = next.entry(name).or_default();
            *longest = (*longest).max(came);
        }
        costs = next;
    }
    cost(&costs, body).1
}

/// Whether a DOCTYPE declares an entity whose value holds markup (a `<`).
pub fn entity_with_markup(decl: &[u8]) -> bool {
    let mut at = 0;
    while let Some(p) = decl.get(at..).and_then(|r| r.windows(8).position(|w| w == b"<!ENTITY")) {
        let start = at + p + 8;
        // The declaration runs to the first `>` outside quotes (a quoted value may hold `>`); a
        // `<` anywhere in it, quoted or not, is markup.
        let mut quote = None::<u8>;
        let mut end = decl.len();
        for (k, &c) in decl.get(start..).unwrap_or(&[]).iter().enumerate() {
            match (quote, c) {
                (_, b'<') => return true,
                (Some(q), c) if c == q => quote = None,
                (Some(_), _) => {}
                (None, b'"' | b'\'') => quote = Some(c),
                (None, b'>') => {
                    end = start + k;
                    break;
                }
                _ => {}
            }
        }
        at = end;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nested(n: usize) -> String {
        format!("<?xml version=\"1.0\"?><r>{}{}</r>", "<a>".repeat(n), "</a>".repeat(n))
    }

    #[test]
    fn depth_is_bounded() {
        assert!(check_limits(&nested(200), false).is_ok());
        assert!(check_limits(&nested(10_000), false).is_err());
        // Self-closed, commented, CDATA and quoted `<`s don't nest.
        let flat = format!(
            "<r>{}<!-- {} --><![CDATA[{}]]><a v=\"{}\"/></r>",
            "<a/>".repeat(1000),
            "<a>".repeat(1000),
            "<a>".repeat(1000),
            "<a>".repeat(300)
        );
        assert!(check_limits(&flat, false).is_ok());
        // Unclosed opens count (a truncated file never reads as shallow).
        assert!(check_limits(&"<a>".repeat(300), false).is_err());
        let (deep, shallow) = (nested(10_000), nested(10));
        assert!(parse(&deep, ParsingOptions::default()).is_err());
        assert!(parse(&shallow, ParsingOptions::default()).is_ok());
        // An entity holding markup, with a DTD allowed: refused; plain ones are fine.
        let ent = "<!DOCTYPE r [<!ENTITY a \"<b><b>&a;</b></b>\">]><r>&a;</r>";
        assert!(check_limits(ent, true).is_err());
        // A `>` in the quoted value before its markup doesn't hide the markup.
        for hidden in ["<!DOCTYPE r [<!ENTITY a \"x>&b;<c>\">]><r/>", "<!DOCTYPE r [<!ENTITY a 'x>y<c>'>]><r/>"] {
            assert!(check_limits(hidden, true).is_err(), "{hidden}");
        }
        // A comment or instruction in the subset with an apostrophe doesn't hide the document
        // after it: its nesting is still counted.
        for subset in ["<!-- it's -->", "<?pi it's?>"] {
            let doc = format!("<!DOCTYPE r [ {subset} ]>{}", nested(10_000));
            assert!(check_limits(&doc, true).is_err() && check_limits(&doc, false).is_err(), "{subset}");
            let ok = format!("<!DOCTYPE r [ {subset} ]><r/>");
            assert!(check_limits(&ok, true).is_ok(), "{subset}");
        }
        // A DOCTYPE that never ends is refused rather than skipping the rest.
        assert!(check_limits(&format!("<!DOCTYPE r [ '{}", nested(10)), true).is_err());
        let plain =
            "<!DOCTYPE WMT_MS_Capabilities SYSTEM \"http://x/WMS_MS_Capabilities.dtd\" [<!ELEMENT VendorSpecificCapabilities EMPTY>]><r/>";
        assert!(check_limits(plain, true).is_ok());
    }

    /// `n` namespace declarations, `n0` on.
    fn declarations(n: usize) -> String {
        (0..n).map(|k| format!(r#" xmlns:n{k}="urn:{k}""#)).collect()
    }

    /// Namespaces declared around many elements make resolving them grow as their product: each
    /// way of building that is refused before parsing, while real documents pass.
    #[test]
    fn namespace_work_is_bounded() {
        // Each is refused for its own kind of work: the root's own declarations are few enough
        // to pass alone.
        // Copied: a few hundred namespaces around children that each declare one, so each copies
        // them all in, checking each against those copied before it; declared as the default
        // namespace by an attribute named `xmlns` with a prefix too, as the parser reads it.
        let copied = format!("<r{}>{}</r>", declarations(300), r#"<a xmlns:x="urn:x"/>"#.repeat(5000));
        let qualified = format!("<r{}>{}</r>", declarations(300), r#"<a n0:xmlns="urn:x"/>"#.repeat(5000));
        // Checked against each other: one element declaring many.
        let one = format!("<r{}/>", declarations(20_000));
        // Looked up: a root declaring many, around many prefixed elements declaring none.
        let looked_up = format!("<r{}>{}</r>", declarations(1000), "<n0:a/>".repeat(20_000));
        // Attributes checked against each other: one element with many.
        let attributes: String = (0..20_000).map(|k| format!(" a{k}=''")).collect();
        let attributed = format!("<r{attributes}/>");
        for doc in [&copied, &qualified, &one, &looked_up, &attributed] {
            let refused = check_limits(doc, false).unwrap_err();
            assert!(refused.contains("namespace declarations"), "{refused}");
            assert!(parse(doc, ParsingOptions::default()).is_err());
        }
        // A real document: a few dozen namespaces on its root, small elements with prefixed
        // attributes, a prefix declared again inside.
        let real = format!(
            "<r{}>{}</r>",
            declarations(40),
            r#"<n1:a n2:b="1"><n1:c xmlns:n1="urn:1" n3:d='x = "y"'>2</n1:c></n1:a>"#.repeat(20_000)
        );
        assert!(check_limits(&real, false).is_ok());
        assert!(parse(&real, ParsingOptions::default()).is_ok());
        // As many children declaring one, with a few dozen namespaces around them (as a service
        // writing its namespaces on every feature does).
        let few = format!("<r{}>{}</r>", declarations(40), r#"<a xmlns:x="urn:x"/>"#.repeat(5000));
        assert!(check_limits(&few, false).is_ok());
        // A root declaring a few hundred, in a short document.
        assert!(check_limits(&format!("<r{}/>", declarations(500)), false).is_ok());
    }

    /// A DOCTYPE declaring `entities`, around an `<r>` holding `body`.
    fn declaring(entities: &str, body: &str) -> String {
        format!("<!DOCTYPE r [{entities}]><r>{body}</r>")
    }

    /// What entities expand to where they are used is bounded by the document's length: a long one
    /// used many times, or short ones nested into a long one, is refused before parsing (#3232),
    /// while one used often for little, or declared and never used, is read.
    #[test]
    fn entity_expansion_is_bounded() {
        let long = format!(r#"<!ENTITY e "{}">"#, "a".repeat(100_000));
        let nested = format!(r#"<!ENTITY a "{}"><!ENTITY b "{}"><!ENTITY c "{}">"#, "x".repeat(100), "&a;".repeat(100), "&b;".repeat(200));
        for doc in [
            declaring(&long, &format!(r#"<a v="{}"/>"#, "&e;".repeat(4000))),
            declaring(&long, &"&e;".repeat(4000)),
            declaring(&nested, "&c;"),
        ] {
            assert_eq!(check_limits(&doc, true).unwrap_err(), TOO_MUCH_ENTITY_TEXT);
        }
        let short = format!(r#"<!ENTITY e "{}">"#, "a".repeat(10));
        for doc in [declaring(&short, &"&e;".repeat(500)), declaring(&nested, "&b;"), declaring(&long, "<a/>")] {
            assert!(parse(&doc, ParsingOptions { allow_dtd: true, ..ParsingOptions::default() }).is_ok());
        }
        // Declared where the parser reads them: as a parameter entity, which it expands as any
        // other, or after a declaration it ends at a `>` inside quotes.
        let uses = format!(r#"<r v="{}"/>"#, "&e;".repeat(4000));
        let value = "a".repeat(100_000);
        for subset in
            [format!(r#"<!ENTITY % e "{value}">"#), format!(r#"<!ATTLIST r x CDATA "> <!ENTITY e '{value}'> <!ATTLIST r y CDATA "> "#)]
        {
            let doc = format!("<!DOCTYPE r [{subset}]>{uses}");
            assert_eq!(check_limits(&doc, true).unwrap_err(), TOO_MUCH_ENTITY_TEXT);
        }
        // Each use also costs the parser a search among the declarations, one by one.
        let many: String = (0..20_000).map(|i| format!(r#"<!ENTITY e{i} "a">"#)).collect();
        assert_eq!(check_limits(&declaring(&many, &"&e19999;".repeat(20_000)), true).unwrap_err(), TOO_MUCH_ENTITY_TEXT);
        // A reference within a value counts as what it expands to, not as its own text too.
        let name = "n".repeat(100);
        let named = format!(r#"<!ENTITY {name} "x"><!ENTITY b "&{name};">"#);
        assert!(check_limits(&declaring(&named, &"&b;".repeat(12_000)), true).is_ok());
        // And a long name is read again each time a value that holds its reference is expanded.
        let long_name = "n".repeat(100_000);
        let reread = format!(r#"<!ENTITY {long_name} "x"><!ENTITY b "&{long_name};">"#);
        assert_eq!(check_limits(&declaring(&reread, &"&b;".repeat(20_000)), true).unwrap_err(), TOO_MUCH_ENTITY_TEXT);
        // Counted in time that grows with the document's length, however it is broken.
        let started = std::time::Instant::now();
        for doc in [format!("<!DOCTYPE r [{} ]><r/>", "<!ENTITY".repeat(50_000)), "<!x [<!ENTITY a ''>]>".repeat(50_000)] {
            // Refused or not, which the parser decides: only the time is looked at.
            let _ = check_limits(&doc, true);
        }
        assert!(started.elapsed() < std::time::Duration::from_secs(1), "{:?}", started.elapsed());
        // A service's capabilities are held to it too.
        let capabilities = format!(r#"<!DOCTYPE WMS_Capabilities [{long}]><WMS_Capabilities v="{}"/>"#, "&e;".repeat(4000));
        let refused = crate::ogc::parse("https://x.example/wms", capabilities.as_bytes()).unwrap_err();
        assert!(refused.to_string().contains(TOO_MUCH_ENTITY_TEXT), "{refused}");
    }

    /// Every XML reader refuses a document whose namespaces would take too long to resolve.
    #[test]
    fn every_reader_refuses_namespace_floods() {
        let flood = |root: &str| format!("<?xml version=\"1.0\"?><{root}{}/>", declarations(20_000)).into_bytes();
        let said = |e: String| e.contains("namespace declarations");
        assert!(crate::gpx::read("a.gpx", &flood("gpx")).is_err_and(|e| said(e.to_string())));
        assert!(crate::kml::read("a.kml", &flood("kml")).is_err_and(|e| said(e.to_string())));
        assert!(crate::gml::read("a.gml", &flood("FeatureCollection")).is_err_and(|e| said(e.to_string())));
        assert!(crate::georss::read("a.rss", &flood("rss")).is_err_and(|e| said(e.to_string())));
        assert!(crate::landxml::read("a.xml", &flood("LandXML"), None).is_err_and(|e| said(e.to_string())));
        assert!(crate::ogc::parse("https://x.example/wms", &flood("WMS_Capabilities")).is_err_and(|e| said(e.to_string())));
    }

    /// Every XML reader refuses a deeply nested document instead of overflowing the stack, run on
    /// a small one (debug builds use the most stack per frame).
    #[test]
    fn every_reader_refuses_deep_nesting() {
        let r = std::thread::Builder::new()
            .stack_size(512 << 10)
            .spawn(|| {
                let deep = |root: &str, inner: &str| {
                    format!(
                        "<?xml version=\"1.0\"?><{root}>{}{}</{root}>",
                        format!("<{inner}>").repeat(10_000),
                        format!("</{inner}>").repeat(10_000)
                    )
                    .into_bytes()
                };
                assert!(crate::gpx::read("a.gpx", &deep("gpx", "trk")).is_err());
                assert!(crate::kml::read("a.kml", &deep("kml", "Folder")).is_err());
                assert!(crate::kml::ground_overlays("a.kml", &deep("kml", "Folder")).is_err());
                assert!(crate::gml::read("a.gml", &deep("wfs:FeatureCollection", "gml:featureMember")).is_err());
                assert!(crate::georss::read("a.rss", &deep("rss", "channel")).is_err());
                assert!(crate::landxml::read("a.xml", &deep("LandXML", "Parcels"), None).is_err());
                assert!(crate::ogc::parse("https://x.example/wms", &deep("WMS_Capabilities", "Layer")).is_err());
                true
            })
            .unwrap()
            .join()
            .unwrap();
        assert!(r);
    }

    /// A deep document is refused on a small stack (as a thread of the app's might have), never
    /// overflowing it.
    #[test]
    fn deep_documents_never_overflow_a_small_stack() {
        let text = nested(100_000);
        let r = std::thread::Builder::new()
            .stack_size(256 << 10)
            .spawn(move || parse(&text, ParsingOptions::default()).is_err())
            .unwrap()
            .join()
            .unwrap();
        assert!(r);
    }
}
