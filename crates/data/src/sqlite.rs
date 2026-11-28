//! A read-only reader of SQLite database files, in pure Rust: table b-trees, records and overflow
//! pages, and the column lists of `CREATE TABLE` statements. It is what the browser opens
//! GeoPackages with (SQLite itself is C); the desktop uses SQLite.
//!
//! No SQL is run: tables are scanned in rowid order. The file is hostile: every page number,
//! offset and length is checked against the bytes there, a page is visited at most once per scan
//! (so a cycle of pages ends), and a malformed page is an error, never a panic.

use std::ops::ControlFlow;

/// A value in a row, as stored: SQLite stores a whole-number REAL as an integer, so readers
/// convert by the column's declared type.
#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
    /// Text whose bytes aren't valid UTF-8, kept as they are: a reader that says so can count it
    /// ([`Val::text`] reads it with U+FFFD in their place).
    BadText(Vec<u8>),
    Blob(Vec<u8>),
}

impl Val {
    /// The text this holds, a [`Val::BadText`]'s with U+FFFD in place of the bytes that aren't UTF-8.
    pub fn text(&self) -> Option<std::borrow::Cow<'_, str>> {
        match self {
            Val::Text(t) => Some(std::borrow::Cow::Borrowed(t)),
            Val::BadText(b) => Some(String::from_utf8_lossy(b)),
            _ => None,
        }
    }
}

/// A column of a table, as its `CREATE TABLE` declares it.
#[derive(Clone, Debug, PartialEq)]
pub struct Column {
    pub name: String,
    /// The declared type (`INTEGER`, `TEXT`, `POINT`…), possibly empty.
    pub decl: String,
    /// A virtual generated column: not stored in the rows.
    pub generated: bool,
}

/// A table of the database.
#[derive(Clone, Debug, PartialEq)]
pub struct Table {
    pub name: String,
    /// The root page of its b-tree.
    pub root: u32,
    pub columns: Vec<Column>,
    /// The `INTEGER PRIMARY KEY` column, which is the rowid (stored as NULL in the rows).
    pub rowid_column: Option<usize>,
    /// For a `WITHOUT ROWID` table (stored as an index b-tree, primary key first), the column of
    /// each value in a stored row.
    pub stored_order: Option<Vec<usize>>,
}

/// An error reading the file.
fn bad(what: &str) -> String {
    format!("not a readable SQLite database: {what}")
}

/// An SQLite database held in memory.
pub struct Db<B: AsRef<[u8]>> {
    bytes: B,
    page_size: usize,
    /// Bytes of each page the b-tree uses (the page size less the reserved bytes at its end).
    usable: usize,
    pages: u32,
}

impl<B: AsRef<[u8]>> Db<B> {
    /// Opens the database in `bytes`, checking its header.
    pub fn open(bytes: B) -> Result<Self, String> {
        let b = bytes.as_ref();
        if b.get(..16) != Some(b"SQLite format 3\0") {
            return Err("not an SQLite database".into());
        }
        let page_size = match be16(b, 16).ok_or_else(|| bad("header"))? {
            1 => 65_536,
            n if n >= 512 && n.is_power_of_two() => n,
            _ => return Err(bad("page size")),
        };
        let reserved = usize::from(*b.get(20).ok_or_else(|| bad("header"))?);
        let usable = page_size.checked_sub(reserved).filter(|&u| u >= 480).ok_or_else(|| bad("reserved space"))?;
        match be32(b, 56).ok_or_else(|| bad("header"))? {
            0 | 1 => {}
            _ => return Err("SQLite databases in UTF-16 aren't read".into()),
        }
        let pages = u32::try_from(b.len() / page_size).map_err(|_| bad("size"))?;
        if pages == 0 {
            return Err(bad("no pages"));
        }
        Ok(Db { bytes, page_size, usable, pages })
    }

    /// Page `n` (numbered from 1).
    fn page(&self, n: u32) -> Result<&[u8], String> {
        if n == 0 || n > self.pages {
            return Err(bad("a page number past the end of the file"));
        }
        let start = (n as usize - 1).checked_mul(self.page_size).ok_or_else(|| bad("page offset"))?;
        let end = start.checked_add(self.page_size).ok_or_else(|| bad("page offset"))?;
        self.bytes.as_ref().get(start..end).ok_or_else(|| bad("a truncated page"))
    }

    /// Calls `f` with the rowid and values of every row of the table b-tree rooted at `root`, in
    /// rowid order, until it returns `Break`.
    pub fn scan(&self, root: u32, mut f: impl FnMut(i64, Vec<Val>) -> ControlFlow<()>) -> Result<(), String> {
        let mut seen = vec![false; self.pages as usize + 1];
        // A page (b-tree or overflow) is read once per scan, so payloads that share an overflow
        // chain — which only a forged file has — are refused rather than read again and again.
        let mut stack = vec![root];
        // Depth-first, children pushed right to left so rows come out in rowid order.
        while let Some(n) = stack.pop() {
            let slot = seen.get_mut(n as usize).ok_or_else(|| bad("a page number past the end of the file"))?;
            if std::mem::replace(slot, true) {
                return Err(bad("a page reached twice"));
            }
            let page = self.page(n)?;
            let hdr = if n == 1 { 100 } else { 0 };
            let kind = *page.get(hdr).ok_or_else(|| bad("page header"))?;
            let cells = be16(page, hdr + 3).ok_or_else(|| bad("page header"))?;
            match kind {
                // An interior table page: child pointers, then the right-most child.
                0x05 => {
                    let mut children = Vec::with_capacity(cells + 1);
                    for i in 0..cells {
                        let at = be16(page, hdr + 12 + 2 * i).ok_or_else(|| bad("cell pointer"))?;
                        children.push(be32(page, at).ok_or_else(|| bad("interior cell"))?);
                    }
                    children.push(be32(page, hdr + 8).ok_or_else(|| bad("page header"))?);
                    stack.extend(children.into_iter().rev());
                }
                // A leaf table page: (payload size, rowid, payload) cells.
                0x0d => {
                    for i in 0..cells {
                        let at = be16(page, hdr + 8 + 2 * i).ok_or_else(|| bad("cell pointer"))?;
                        let (size, a) = varint(page, at).ok_or_else(|| bad("leaf cell"))?;
                        let (rowid, a) = varint(page, a).ok_or_else(|| bad("leaf cell"))?;
                        let payload = self.payload_seen(page, a, size, &mut seen)?;
                        if f(rowid as i64, record(&payload)?).is_break() {
                            return Ok(());
                        }
                    }
                }
                _ => return Err(bad("a table page of the wrong kind")),
            }
        }
        Ok(())
    }

    /// Calls `f` with the values of every entry of the index b-tree rooted at `root` (a
    /// `WITHOUT ROWID` table's rows), in key order, until it returns `Break`.
    pub fn scan_index(&self, root: u32, mut f: impl FnMut(Vec<Val>) -> ControlFlow<()>) -> Result<(), String> {
        enum Step {
            Page(u32),
            /// An interior cell's own entry, which sits between two children.
            Entry(Vec<u8>),
        }
        let mut seen = vec![false; self.pages as usize + 1];
        let max_local = (self.usable - 12) * 64 / 255 - 23;
        let mut stack = vec![Step::Page(root)];
        while let Some(step) = stack.pop() {
            let n = match step {
                Step::Page(n) => n,
                Step::Entry(payload) => {
                    if f(record(&payload)?).is_break() {
                        return Ok(());
                    }
                    continue;
                }
            };
            let slot = seen.get_mut(n as usize).ok_or_else(|| bad("a page number past the end of the file"))?;
            if std::mem::replace(slot, true) {
                return Err(bad("a page reached twice"));
            }
            let page = self.page(n)?;
            let hdr = if n == 1 { 100 } else { 0 };
            let kind = *page.get(hdr).ok_or_else(|| bad("page header"))?;
            let cells = be16(page, hdr + 3).ok_or_else(|| bad("page header"))?;
            match kind {
                // An interior index page: (left child, payload size, payload) cells, then the
                // right-most child. Each cell's entry comes after its left child's.
                0x02 => {
                    let mut steps = Vec::with_capacity(2 * cells + 1);
                    for i in 0..cells {
                        let at = be16(page, hdr + 12 + 2 * i).ok_or_else(|| bad("cell pointer"))?;
                        steps.push(Step::Page(be32(page, at).ok_or_else(|| bad("interior cell"))?));
                        let (size, a) = varint(page, at + 4).ok_or_else(|| bad("interior cell"))?;
                        steps.push(Step::Entry(self.payload_with(page, a, size, max_local, Some(&mut seen))?));
                    }
                    steps.push(Step::Page(be32(page, hdr + 8).ok_or_else(|| bad("page header"))?));
                    stack.extend(steps.into_iter().rev());
                }
                // A leaf index page: (payload size, payload) cells.
                0x0a => {
                    for i in 0..cells {
                        let at = be16(page, hdr + 8 + 2 * i).ok_or_else(|| bad("cell pointer"))?;
                        let (size, a) = varint(page, at).ok_or_else(|| bad("leaf cell"))?;
                        let payload = self.payload_with(page, a, size, max_local, Some(&mut seen))?;
                        if f(record(&payload)?).is_break() {
                            return Ok(());
                        }
                    }
                }
                _ => return Err(bad("an index page of the wrong kind")),
            }
        }
        Ok(())
    }

    /// The number of rows in the table b-tree rooted at `root`, from the leaf pages' cell counts
    /// (no row is decoded).
    pub fn count(&self, root: u32) -> Result<u64, String> {
        let mut seen = vec![false; self.pages as usize + 1];
        let mut stack = vec![root];
        let mut n_rows = 0u64;
        while let Some(n) = stack.pop() {
            let slot = seen.get_mut(n as usize).ok_or_else(|| bad("a page number past the end of the file"))?;
            if std::mem::replace(slot, true) {
                return Err(bad("a page reached twice"));
            }
            let page = self.page(n)?;
            let hdr = if n == 1 { 100 } else { 0 };
            let cells = be16(page, hdr + 3).ok_or_else(|| bad("page header"))?;
            match page.get(hdr) {
                Some(0x05) => {
                    for i in 0..cells {
                        let at = be16(page, hdr + 12 + 2 * i).ok_or_else(|| bad("cell pointer"))?;
                        stack.push(be32(page, at).ok_or_else(|| bad("interior cell"))?);
                    }
                    stack.push(be32(page, hdr + 8).ok_or_else(|| bad("page header"))?);
                }
                Some(0x0d) => n_rows += cells as u64,
                _ => return Err(bad("a table page of the wrong kind")),
            }
        }
        Ok(n_rows)
    }

    /// A cell's payload of `size` bytes starting at `at` on `page`, following its overflow pages.
    /// [`Db::payload`] during a scan: each overflow page may be read only once.
    fn payload_seen(&self, page: &[u8], at: usize, size: u64, seen: &mut [bool]) -> Result<Vec<u8>, String> {
        self.payload_with(page, at, size, self.usable - 35, Some(seen))
    }

    fn payload_with(&self, page: &[u8], at: usize, size: u64, max_local: usize, mut seen: Option<&mut [bool]>) -> Result<Vec<u8>, String> {
        let total = self.bytes.as_ref().len() as u64;
        if size > total {
            return Err(bad("a payload larger than the file"));
        }
        let size = size as usize;
        let u = self.usable;
        // How much of the payload is on the page itself (the SQLite file format's rule).
        let local = if size <= max_local {
            size
        } else {
            let min_local = (u - 12) * 32 / 255 - 23;
            let k = min_local + (size - min_local) % (u - 4);
            if k <= max_local { k } else { min_local }
        };
        // Grown as pages are read: a forged size can't make a large allocation up front.
        let mut out = Vec::with_capacity(size.min(local + 4096));
        out.extend_from_slice(page.get(at..at + local).ok_or_else(|| bad("a cell past its page"))?);
        if local < size {
            let mut next = be32(page, at + local).ok_or_else(|| bad("overflow pointer"))?;
            let mut hops = 0u32;
            while out.len() < size {
                hops += 1;
                if next == 0 || hops > self.pages {
                    return Err(bad("an overflow chain that ends early"));
                }
                if let Some(seen) = seen.as_deref_mut() {
                    let slot = seen.get_mut(next as usize).ok_or_else(|| bad("a page number past the end of the file"))?;
                    if std::mem::replace(slot, true) {
                        return Err(bad("an overflow page reached twice"));
                    }
                }
                let p = self.page(next)?;
                let take = (size - out.len()).min(u - 4);
                out.extend_from_slice(p.get(4..4 + take).ok_or_else(|| bad("overflow page"))?);
                next = be32(p, 0).ok_or_else(|| bad("overflow page"))?;
            }
        }
        Ok(out)
    }

    /// The tables, from the schema table (`sqlite_schema`, page 1).
    pub fn tables(&self) -> Result<Vec<Table>, String> {
        let mut out = Vec::new();
        let mut err = None;
        self.scan(1, |_, row| {
            // Text that isn't UTF-8 reads with U+FFFD, as it always did: such a table is still listed.
            if let [kind, name, _, Val::Int(root), sql, ..] = row.as_slice()
                && let (Some(kind), Some(name), Some(sql)) = (kind.text(), name.text(), sql.text())
                && kind == "table"
            {
                // Unparsable tables are passed over; another may be wanted.
                if let Ok(p) = parse_table(&sql) {
                    match u32::try_from(*root) {
                        // A virtual table (an R-tree index, say) has no pages of its own.
                        Ok(0) => {}
                        Ok(root) => out.push(Table {
                            name: name.to_string(),
                            root,
                            columns: p.columns,
                            rowid_column: p.rowid_column,
                            stored_order: p.stored_order,
                        }),
                        Err(_) => err = Some(bad("a table's root page")),
                    }
                }
            }
            ControlFlow::Continue(())
        })?;
        match err {
            Some(e) => Err(e),
            None => Ok(out),
        }
    }

    /// A table by name, without regard to case (as SQLite looks names up).
    pub fn table(&self, name: &str) -> Result<Table, String> {
        self.tables()?.into_iter().find(|t| t.name.eq_ignore_ascii_case(name)).ok_or_else(|| format!("no table '{name}'"))
    }

    /// Calls `f` with each row of `table` as one value per column (the rowid in its
    /// `INTEGER PRIMARY KEY` column; columns added after the row was written are NULL), until it
    /// returns `Break`. A `WITHOUT ROWID` table's rows come in primary-key order, numbered from 1
    /// in place of a rowid.
    pub fn rows(&self, table: &Table, mut f: impl FnMut(i64, &[Val]) -> ControlFlow<()>) -> Result<(), String> {
        let mut row = Vec::with_capacity(table.columns.len());
        if let Some(order) = &table.stored_order {
            let mut n = 0i64;
            return self.scan_index(table.root, |stored| {
                n += 1;
                row.clear();
                row.resize(table.columns.len(), Val::Null);
                for (v, &col) in stored.into_iter().zip(order) {
                    if let Some(slot) = row.get_mut(col) {
                        *slot = v;
                    }
                }
                f(n, &row)
            });
        }
        self.scan(table.root, |rowid, stored| {
            row.clear();
            let mut stored = stored.into_iter();
            for (i, c) in table.columns.iter().enumerate() {
                let v = if c.generated { Val::Null } else { stored.next().unwrap_or(Val::Null) };
                row.push(if Some(i) == table.rowid_column { Val::Int(rowid) } else { v });
            }
            f(rowid, &row)
        })
    }
}

fn be16(b: &[u8], at: usize) -> Option<usize> {
    let s = b.get(at..at.checked_add(2)?)?;
    Some(usize::from(u16::from_be_bytes([*s.first()?, *s.get(1)?])))
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    let s: [u8; 4] = b.get(at..at.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_be_bytes(s))
}

/// An SQLite varint at `at`: the value and the offset after it.
fn varint(b: &[u8], at: usize) -> Option<(u64, usize)> {
    let mut v = 0u64;
    for i in 0..9 {
        let byte = *b.get(at.checked_add(i)?)?;
        if i == 8 {
            return Some(((v << 8) | u64::from(byte), at + 9));
        }
        v = (v << 7) | u64::from(byte & 0x7f);
        if byte & 0x80 == 0 {
            return Some((v, at + i + 1));
        }
    }
    None
}

/// The values of a record: a header of serial types, then the values.
fn record(p: &[u8]) -> Result<Vec<Val>, String> {
    let (header_len, mut h) = varint(p, 0).ok_or_else(|| bad("record header"))?;
    let header_len = usize::try_from(header_len).ok().filter(|&n| n <= p.len()).ok_or_else(|| bad("record header"))?;
    let mut body = header_len;
    let mut out = Vec::new();
    while h < header_len {
        let (t, next) = varint(p, h).ok_or_else(|| bad("record header"))?;
        h = next;
        let int = |n: usize| -> Result<i64, String> {
            let s = p.get(body..body + n).ok_or_else(|| bad("a record past its payload"))?;
            // Big-endian two's complement of 1 to 8 bytes, sign-extended.
            let first = i64::from(*s.first().ok_or_else(|| bad("record"))? as i8);
            Ok(s.iter().skip(1).fold(first, |v, &b| (v << 8) | i64::from(b)))
        };
        let (v, len) = match t {
            0 => (Val::Null, 0),
            1..=4 => (Val::Int(int(t as usize)?), t as usize),
            5 => (Val::Int(int(6)?), 6),
            6 => (Val::Int(int(8)?), 8),
            7 => {
                let s: [u8; 8] = p.get(body..body + 8).and_then(|s| s.try_into().ok()).ok_or_else(|| bad("a record past its payload"))?;
                (Val::Real(f64::from_be_bytes(s)), 8)
            }
            8 => (Val::Int(0), 0),
            9 => (Val::Int(1), 0),
            10 | 11 => return Err(bad("a reserved serial type")),
            t => {
                let n = usize::try_from((t - 12) / 2).map_err(|_| bad("record"))?;
                let s = p.get(body..body.checked_add(n).ok_or_else(|| bad("record"))?).ok_or_else(|| bad("a record past its payload"))?;
                (
                    match (t % 2 == 0, std::str::from_utf8(s)) {
                        (true, _) => Val::Blob(s.to_vec()),
                        (false, Ok(text)) => Val::Text(text.to_string()),
                        (false, Err(_)) => Val::BadText(s.to_vec()),
                    },
                    n,
                )
            }
        };
        body += len;
        out.push(v);
    }
    Ok(out)
}

/// The columns of a `CREATE TABLE` statement and the index of its `INTEGER PRIMARY KEY` column.
/// `WITHOUT ROWID` tables (whose rows are stored differently) are an error.
pub fn parse_create_table(sql: &str) -> Result<(Vec<Column>, Option<usize>), String> {
    let p = parse_table(sql)?;
    if p.stored_order.is_some() {
        return Err("a WITHOUT ROWID table".into());
    }
    Ok((p.columns, p.rowid_column))
}

/// A `CREATE TABLE` statement, read.
struct Parsed {
    columns: Vec<Column>,
    rowid_column: Option<usize>,
    stored_order: Option<Vec<usize>>,
}

/// Reads a `CREATE TABLE` statement, rowid or `WITHOUT ROWID`.
fn parse_table(sql: &str) -> Result<Parsed, String> {
    let (open, close) = column_list(sql).ok_or("no column list")?;
    // The words after the column list, comments dropped (`-- note` then `WITHOUT ROWID` on the
    // next line is still one).
    let tail = tokens(sql.get(close + 1..).unwrap_or(""));
    let without_rowid = tail.iter().any(|t| !t.quoted && t.text.eq_ignore_ascii_case("ROWID"));
    let mut column_pk: Option<usize> = None;
    let body = sql.get(open + 1..close).ok_or("no column list")?;
    let mut columns = Vec::new();
    let mut rowid_column = None;
    let mut table_pk: Option<Vec<String>> = None;
    for def in split_top(body) {
        let toks = tokens(&def);
        let Some(first) = toks.first() else { continue };
        let up = |s: &str| s.to_ascii_uppercase();
        if !first.quoted && matches!(up(&first.text).as_str(), "CONSTRAINT" | "PRIMARY" | "UNIQUE" | "CHECK" | "FOREIGN") {
            // A table constraint; a PRIMARY KEY (one column) may name the rowid's alias.
            if let Some(p) = toks.iter().position(|t| !t.quoted && up(&t.text) == "PRIMARY")
                && toks.get(p + 1).is_some_and(|t| !t.quoted && up(&t.text) == "KEY")
                && let Some(open) = toks[p + 2..].iter().position(|t| !t.quoted && t.text == "(")
            {
                let open_idx = p + 2 + open;
                let mut depth = 0usize;
                let mut close_idx = None;
                for (i, t) in toks.iter().enumerate().skip(open_idx) {
                    if !t.quoted && t.text == "(" {
                        depth += 1;
                    } else if !t.quoted && t.text == ")" {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            close_idx = Some(i);
                            break;
                        }
                    }
                }
                if let Some(close) = close_idx {
                    let mut cols = Vec::new();
                    let mut in_entry = false;
                    let mut depth = 0usize;
                    for t in &toks[open_idx + 1..close] {
                        if !t.quoted && t.text == "(" {
                            depth += 1;
                        } else if !t.quoted && t.text == ")" {
                            depth = depth.saturating_sub(1);
                        } else if !t.quoted && t.text == "," && depth == 0 {
                            in_entry = false;
                        } else if !in_entry && depth == 0 {
                            cols.push(t.text.clone());
                            in_entry = true;
                        }
                    }
                    table_pk = Some(cols);
                }
            }
            continue;
        }
        const STOP: &[&str] =
            &["CONSTRAINT", "PRIMARY", "NOT", "NULL", "UNIQUE", "CHECK", "DEFAULT", "COLLATE", "REFERENCES", "GENERATED", "AS"];
        let ty: Vec<&str> =
            toks.iter().skip(1).take_while(|t| t.quoted || !STOP.contains(&up(&t.text).as_str())).map(|t| t.text.as_str()).collect();
        let decl = ty.join(" ").replace(" (", "(").replace("( ", "(").replace(" )", ")").replace(" ,", ",");
        let words: Vec<String> = toks.iter().filter(|t| !t.quoted).map(|t| up(&t.text)).collect();
        let has = |w: &str| words.iter().any(|x| x == w);
        // A generated column: `GENERATED ALWAYS AS (…)` or `AS (…)` after the name, outside any
        // parentheses (a CHECK's `CAST(x AS INT)` isn't one); stored only when declared STORED.
        let mut depth = 0usize;
        let mut generated_kw = false;
        for (k, t) in toks.iter().enumerate().skip(1) {
            match (t.quoted, t.text.as_str()) {
                (false, "(") => depth += 1,
                (false, ")") => depth = depth.saturating_sub(1),
                (false, w) if depth == 0 => {
                    let w = up(w);
                    let next_open = toks.get(k + 1).is_some_and(|n| !n.quoted && n.text == "(");
                    if w == "GENERATED" || (w == "AS" && next_open) {
                        generated_kw = true;
                    }
                }
                _ => {}
            }
        }
        let generated = generated_kw && !has("STORED");
        if has("PRIMARY") {
            column_pk = Some(columns.len());
        }
        if decl.eq_ignore_ascii_case("INTEGER") && has("PRIMARY") && !has("DESC") {
            rowid_column = Some(columns.len());
        }
        columns.push(Column { name: first.text.clone(), decl, generated });
    }
    if columns.is_empty() {
        return Err("no columns".into());
    }
    if without_rowid {
        // Stored primary key first (in its order), then the other columns as declared; virtual
        // generated columns aren't stored.
        let pk: Vec<usize> = match &table_pk {
            Some(names) => names.iter().map(|n| columns.iter().position(|c| c.name.eq_ignore_ascii_case(n))).collect::<Option<_>>(),
            None => column_pk.map(|i| vec![i]),
        }
        .ok_or("a WITHOUT ROWID table without a readable primary key")?;
        let mut order = pk.clone();
        order.extend((0..columns.len()).filter(|i| !pk.contains(i) && columns.get(*i).is_some_and(|c| !c.generated)));
        return Ok(Parsed { columns, rowid_column: None, stored_order: Some(order) });
    }
    if rowid_column.is_none()
        && let Some([pk]) = table_pk.as_deref()
    {
        rowid_column = columns.iter().position(|c| c.name.eq_ignore_ascii_case(pk) && c.decl.eq_ignore_ascii_case("INTEGER"));
    }
    Ok(Parsed { columns, rowid_column, stored_order: None })
}

/// The byte offsets of the parentheses around a statement's column list: the first `(` outside
/// quotes and comments, and the `)` that closes it (a table named `"a(b"` doesn't fool it).
fn column_list(sql: &str) -> Option<(usize, usize)> {
    let b = sql.as_bytes();
    let (mut i, mut depth, mut open) = (0usize, 0usize, None);
    while let Some(&c) = b.get(i) {
        match c {
            b'"' | b'\'' | b'`' | b'[' => {
                let close = if c == b'[' { b']' } else { c };
                i += 1;
                while b.get(i).is_some_and(|&d| d != close) {
                    i += 1;
                }
            }
            b'-' if b.get(i + 1) == Some(&b'-') => {
                while b.get(i).is_some_and(|&d| d != b'\n') {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < b.len() && !(b.get(i) == Some(&b'*') && b.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i += 1;
            }
            b'(' => {
                if depth == 0 && open.is_none() {
                    open = Some(i);
                }
                depth += 1;
            }
            b')' => {
                depth = depth.saturating_sub(1);
                if depth == 0
                    && let Some(o) = open
                {
                    return Some((o, i));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Splits on commas outside parentheses and quotes.
fn split_top(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    for c in s.chars() {
        match quote {
            Some(q) => {
                cur.push(c);
                if c == q || (q == '[' && c == ']') {
                    quote = None;
                }
            }
            None => match c {
                '"' | '\'' | '`' | '[' => {
                    quote = Some(c);
                    cur.push(c);
                }
                '(' => {
                    depth += 1;
                    cur.push(c);
                }
                ')' => {
                    depth = depth.saturating_sub(1);
                    cur.push(c);
                }
                ',' if depth == 0 => out.push(std::mem::take(&mut cur)),
                c => cur.push(c),
            },
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

struct Token {
    text: String,
    quoted: bool,
}

/// Words, quoted names (unquoted, with doubled quotes undone) and punctuation of a definition;
/// `--` and `/* */` comments are dropped.
fn tokens(s: &str) -> Vec<Token> {
    let cs: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(&c) = cs.get(i) {
        if c.is_whitespace() {
            i += 1;
        } else if c == '-' && cs.get(i + 1) == Some(&'-') {
            while cs.get(i).is_some_and(|&c| c != '\n') {
                i += 1;
            }
        } else if c == '/' && cs.get(i + 1) == Some(&'*') {
            i += 2;
            while i < cs.len() && !(cs.get(i) == Some(&'*') && cs.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i += 2;
        } else if matches!(c, '"' | '\'' | '`' | '[') {
            let close = if c == '[' { ']' } else { c };
            let mut text = String::new();
            i += 1;
            while let Some(&d) = cs.get(i) {
                if d == close {
                    if close != ']' && cs.get(i + 1) == Some(&close) {
                        text.push(d);
                        i += 2;
                        continue;
                    }
                    break;
                }
                text.push(d);
                i += 1;
            }
            i += 1;
            out.push(Token { text, quoted: true });
        } else if c.is_alphanumeric() || c == '_' || c == '$' {
            let start = i;
            while cs.get(i).is_some_and(|&d| d.is_alphanumeric() || d == '_' || d == '$') {
                i += 1;
            }
            out.push(Token { text: cs.get(start..i).map(|s| s.iter().collect()).unwrap_or_default(), quoted: false });
        } else {
            out.push(Token { text: c.to_string(), quoted: false });
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_end_overflow_returns_an_error() {
        let db = Db { bytes: Vec::<u8>::new(), page_size: usize::MAX, usable: 480, pages: 2 };
        assert_eq!(db.page(2).unwrap_err(), bad("page offset"));
    }

    #[test]
    fn create_table_columns() {
        let (cols, pk) = parse_create_table(
            r#"CREATE TABLE "my ""t""" ( fid INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL, "geom" MULTIPOLYGON CHECK (CAST(1 AS INT) > 0), [name] TEXT(80) DEFAULT 'a,b', `n` DOUBLE PRECISION, -- note
               x NUMERIC(10, 2), g TEXT GENERATED ALWAYS AS (name || 'x') VIRTUAL, CONSTRAINT c UNIQUE (name))"#,
        )
        .unwrap();
        let names: Vec<&str> = cols.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["fid", "geom", "name", "n", "x", "g"]);
        assert_eq!(cols[1].decl, "MULTIPOLYGON");
        assert_eq!(cols[2].decl, "TEXT(80)");
        assert_eq!(cols[3].decl, "DOUBLE PRECISION");
        assert_eq!(cols[4].decl, "NUMERIC(10, 2)");
        assert!(cols[5].generated && !cols[2].generated);
        assert!(!cols[1].generated, "a CAST … AS inside a CHECK isn't a generated column");
        let (cols, _) = parse_create_table("CREATE TABLE t (generated TEXT, b INT)").unwrap();
        assert!(!cols[0].generated, "a column named generated");
        assert_eq!(pk, Some(0));
        let (_, pk) = parse_create_table("CREATE TABLE t (a TEXT, id INTEGER, PRIMARY KEY (id))").unwrap();
        assert_eq!(pk, Some(1));
        let (_, pk) = parse_create_table("CREATE TABLE t (id INT PRIMARY KEY, b)").unwrap();
        assert_eq!(pk, None, "only INTEGER is the rowid");
        assert!(parse_create_table("CREATE TABLE t (a INTEGER PRIMARY KEY) WITHOUT ROWID").is_err());
        assert!(parse_create_table("CREATE TABLE t").is_err());
        assert!(parse_create_table("CREATE TABLE t (a INTEGER PRIMARY KEY) -- note\n WITHOUT ROWID").is_err());
        assert!(parse_create_table("CREATE TABLE t (a INT) /* not without rowid */").is_ok());
        let (cols, _) = parse_create_table("CREATE TABLE \"a(b\" (x INT, y TEXT) -- note (with parens)").unwrap();
        assert_eq!(cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["x", "y"]);
        let (_, pk) = parse_create_table("CREATE TABLE t (z INTEGER, x INTEGER, y INTEGER, PRIMARY KEY (z ASC, x, y))").unwrap();
        assert_eq!(pk, None, "composite primary key with ASC is not rowid");
        let (_, pk) = parse_create_table("CREATE TABLE t (id INTEGER, other TEXT, PRIMARY KEY (id DESC, other))").unwrap();
        assert_eq!(pk, None, "composite primary key with DESC is not rowid");
        let p = parse_table("CREATE TABLE t (name TEXT, id INTEGER, PRIMARY KEY (name COLLATE NOCASE, id)) WITHOUT ROWID").unwrap();
        assert_eq!(p.stored_order.as_deref(), Some(&[0, 1][..]));
        let p = parse_table("CREATE TABLE shallow (data BLOB, z INTEGER, x INTEGER, y INTEGER, PRIMARY KEY (z ASC, x, y)) WITHOUT ROWID")
            .unwrap();
        assert_eq!(p.stored_order.as_deref(), Some(&[1, 2, 3, 0][..]));
    }

    #[test]
    fn varints_and_records() {
        assert_eq!(varint(&[0x7f], 0), Some((127, 1)));
        assert_eq!(varint(&[0x81, 0x00], 0), Some((128, 2)));
        assert_eq!(varint(&[0xff; 9], 0), Some((u64::MAX, 9)));
        assert_eq!(varint(&[0x81], 0), None);
        // Header of 5 bytes: NULL, a 1-byte int, a 2-byte int, a float, text "hi".
        let mut rec = vec![6, 0, 1, 2, 7, 17, 0xfe, 0x01, 0x00];
        rec.extend_from_slice(&1.5f64.to_be_bytes());
        rec.extend_from_slice(b"hi");
        assert_eq!(record(&rec).unwrap(), vec![Val::Null, Val::Int(-2), Val::Int(256), Val::Real(1.5), Val::Text("hi".into())]);
        assert!(record(&rec[..rec.len() - 1]).is_err());
        assert!(record(&[2, 10]).is_err());
    }

    /// Databases written by SQLite itself: a table spanning interior pages, rows with overflow
    /// pages, a column added after rows were written, small and large pages; every truncation is
    /// an error, not a panic.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn reads_what_sqlite_writes() {
        for page_size in [512, 4096, 65_536] {
            let path = std::env::temp_dir().join(format!("rustgis-sqlite-{}-{page_size}.db", std::process::id()));
            let _ = std::fs::remove_file(&path);
            {
                let c = rusqlite::Connection::open(&path).unwrap();
                c.execute_batch(&format!(
                    "PRAGMA page_size = {page_size}; PRAGMA journal_mode = DELETE;
                     CREATE TABLE \"t\" (id INTEGER PRIMARY KEY, name TEXT, v REAL, b BLOB, n INTEGER);
                     CREATE TABLE other (x);
                     CREATE TABLE comp (id INTEGER, other TEXT, PRIMARY KEY (id DESC, other));
                     INSERT INTO comp VALUES (100, 'first');"
                ))
                .unwrap();
                let tx = c.unchecked_transaction().unwrap();
                for i in 0..3000i64 {
                    // Every 97th row carries a blob larger than a page.
                    let blob: Option<Vec<u8>> = (i % 97 == 0).then(|| (0..page_size * 3 + 7).map(|k| (k % 251) as u8).collect());
                    let n: Option<i64> = (i % 5 != 0).then_some(i * 1_000_000_007);
                    tx.execute(
                        "INSERT INTO t (id, name, v, b, n) VALUES (?1, ?2, ?3, ?4, ?5)",
                        rusqlite::params![i * 3 - 100, format!("row {i} é"), i as f64 / 7.0, blob, n],
                    )
                    .unwrap();
                }
                tx.commit().unwrap();
                c.execute_batch("ALTER TABLE t ADD COLUMN later TEXT; INSERT INTO t (id, name, later) VALUES (999999, 'last', 'x');")
                    .unwrap();
            }
            let bytes = std::fs::read(&path).unwrap();
            let db = Db::open(bytes.as_slice()).unwrap();
            let tables = db.tables().unwrap();
            assert_eq!(tables.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["t", "other", "comp"]);
            let comp = db.table("comp").unwrap();
            assert_eq!(comp.rowid_column, None);
            let mut comp_rows = Vec::new();
            db.rows(&comp, |_, r| {
                comp_rows.push(r.to_vec());
                ControlFlow::Continue(())
            })
            .unwrap();
            assert_eq!(comp_rows[0][0], Val::Int(100));
            let t = db.table("T").unwrap();
            assert_eq!(t.rowid_column, Some(0));
            assert_eq!(t.columns.len(), 6);
            assert_eq!(db.count(t.root).unwrap(), 3001);
            let mut rows = Vec::new();
            db.rows(&t, |_, r| {
                rows.push(r.to_vec());
                ControlFlow::Continue(())
            })
            .unwrap();
            assert_eq!(rows.len(), 3001, "{page_size}");
            for (i, r) in rows.iter().take(3000).enumerate() {
                let i = i as i64;
                assert_eq!(r[0], Val::Int(i * 3 - 100));
                assert_eq!(r[1], Val::Text(format!("row {i} é")));
                // SQLite stores a whole-number REAL as an integer; readers convert by the column's type.
                let v = match r[2] {
                    Val::Real(x) => x,
                    Val::Int(x) => x as f64,
                    ref other => panic!("{other:?}"),
                };
                assert_eq!(v, i as f64 / 7.0);
                let blob = if i % 97 == 0 { Val::Blob((0..page_size * 3 + 7).map(|k| (k % 251) as u8).collect()) } else { Val::Null };
                assert_eq!(r[3], blob, "{page_size} row {i}");
                assert_eq!(r[4], if i % 5 != 0 { Val::Int(i * 1_000_000_007) } else { Val::Null });
                assert_eq!(r[5], Val::Null, "the added column");
            }
            assert_eq!(rows[3000][5], Val::Text("x".into()));
            // Stopping early.
            let mut n = 0;
            db.rows(&t, |_, _| {
                n += 1;
                if n == 10 { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
            })
            .unwrap();
            assert_eq!(n, 10);
            if page_size == 512 {
                for cut in (0..bytes.len()).step_by(211) {
                    if let Ok(db) = Db::open(&bytes[..cut]) {
                        let _ = db.tables();
                        let _ = db.rows(&t, |_, _| ControlFlow::Continue(()));
                    }
                }
                // Flipped bytes in every page.
                let mut junk = bytes.clone();
                for k in (100..junk.len()).step_by(37) {
                    junk[k] ^= 0x5a;
                }
                if let Ok(db) = Db::open(junk.as_slice()) {
                    let _ = db.tables();
                    let _ = db.rows(&t, |_, _| ControlFlow::Continue(()));
                    let _ = db.count(t.root);
                }
            }
            let _ = std::fs::remove_file(&path);
        }
        assert!(Db::open(&b"SQLite format 3\0"[..]).is_err());
        assert!(Db::open(&b"nope"[..]).is_err());
    }

    /// `WITHOUT ROWID` tables (index b-trees): a primary key of several columns, out of column
    /// order, over interior pages, with payloads that overflow; rows come in key order with each
    /// value in its declared column.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn reads_without_rowid_tables() {
        for page_size in [512, 4096] {
            let path = std::env::temp_dir().join(format!("rustgis-sqlite-wr-{}-{page_size}.db", std::process::id()));
            let _ = std::fs::remove_file(&path);
            {
                let c = rusqlite::Connection::open(&path).unwrap();
                c.execute_batch(&format!(
                    "PRAGMA page_size = {page_size}; PRAGMA journal_mode = DELETE;
                     CREATE TABLE shallow (data BLOB, z INTEGER, x INTEGER, y INTEGER, PRIMARY KEY (z ASC, x, y)) WITHOUT ROWID;
                     CREATE TABLE kv (k TEXT PRIMARY KEY, v) WITHOUT ROWID;
                     CREATE TABLE collated (name TEXT, id INTEGER, PRIMARY KEY (name COLLATE NOCASE, id)) WITHOUT ROWID;"
                ))
                .unwrap();
                let tx = c.unchecked_transaction().unwrap();
                for i in 0..2000i64 {
                    // Every 50th row carries a blob larger than a page.
                    let len = if i % 50 == 0 { page_size * 2 + 3 } else { 5 };
                    let blob: Vec<u8> = (0..len).map(|k| ((k as i64 + i) % 251) as u8).collect();
                    tx.execute("INSERT INTO shallow VALUES (?1, ?2, ?3, ?4)", rusqlite::params![blob, i % 7, i, 2000 - i]).unwrap();
                }
                tx.execute("INSERT INTO kv VALUES ('b', 2), ('a', 'one')", []).unwrap();
                tx.execute("INSERT INTO collated VALUES ('alice', 1)", []).unwrap();
                tx.commit().unwrap();
            }
            let bytes = std::fs::read(&path).unwrap();
            let db = Db::open(bytes.as_slice()).unwrap();
            let t = db.table("shallow").unwrap();
            assert_eq!(t.stored_order.as_deref(), Some(&[1, 2, 3, 0][..]));
            assert_eq!(t.rowid_column, None);
            let mut rows = Vec::new();
            db.rows(&t, |n, r| {
                rows.push((n, r.to_vec()));
                ControlFlow::Continue(())
            })
            .unwrap();
            assert_eq!(rows.len(), 2000, "{page_size}");
            let keys: Vec<(i64, i64)> = rows
                .iter()
                .map(|(_, r)| match (&r[1], &r[2]) {
                    (Val::Int(z), Val::Int(x)) => (*z, *x),
                    other => panic!("{other:?}"),
                })
                .collect();
            let mut sorted = keys.clone();
            sorted.sort();
            assert_eq!(keys, sorted, "rows in key order");
            for (n, r) in &rows {
                let Val::Int(i) = r[2] else { panic!() };
                let len = if i % 50 == 0 { page_size * 2 + 3 } else { 5 };
                assert_eq!(r[0], Val::Blob((0..len).map(|k| ((k as i64 + i) % 251) as u8).collect()), "row {n}");
                assert_eq!(r[3], Val::Int(2000 - i));
            }
            let kv = db.table("kv").unwrap();
            let mut got = Vec::new();
            db.rows(&kv, |_, r| {
                got.push(r.to_vec());
                ControlFlow::Continue(())
            })
            .unwrap();
            assert_eq!(got, vec![vec![Val::Text("a".into()), Val::Text("one".into())], vec![Val::Text("b".into()), Val::Int(2)]]);
            let collated = db.table("collated").unwrap();
            assert_eq!(collated.stored_order.as_deref(), Some(&[0, 1][..]));
            let mut collated_rows = Vec::new();
            db.rows(&collated, |_, r| {
                collated_rows.push(r.to_vec());
                ControlFlow::Continue(())
            })
            .unwrap();
            assert_eq!(collated_rows, vec![vec![Val::Text("alice".into()), Val::Int(1)]]);
            if page_size == 512 {
                for cut in (0..bytes.len()).step_by(173) {
                    if let Ok(db) = Db::open(&bytes[..cut]) {
                        let _ = db.rows(&t, |_, _| ControlFlow::Continue(()));
                    }
                }
                let mut junk = bytes.clone();
                for k in (100..junk.len()).step_by(41) {
                    junk[k] ^= 0x3c;
                }
                if let Ok(db) = Db::open(junk.as_slice()) {
                    let _ = db.tables();
                    let _ = db.rows(&t, |_, _| ControlFlow::Continue(()));
                }
            }
            let _ = std::fs::remove_file(&path);
        }
    }
}
