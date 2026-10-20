//! A small length-prefixed binary codec for the persisted history. Decoding never panics:
//! truncated or damaged input is `None`.

use ts_r_diagnostics::{Category, Diagnostic, FileId, RelatedInfo};
use ts_r_span::Span;

use crate::incremental::PathDiag;
use crate::shape::{FileShape, Hash, Provided, Provides, Ref};

#[derive(Debug, Default)]
pub struct Writer {
    pub out: Vec<u8>,
}

impl Writer {
    pub fn u8(&mut self, v: u8) {
        self.out.push(v);
    }
    pub fn u32(&mut self, v: u32) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }
    pub fn u64(&mut self, v: u64) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }
    pub fn count(&mut self, n: usize) {
        self.u32(n as u32);
    }
    pub fn bool(&mut self, v: bool) {
        self.u8(v as u8);
    }
    pub fn str(&mut self, s: &str) {
        self.count(s.len());
        self.out.extend_from_slice(s.as_bytes());
    }
    pub fn hash(&mut self, h: &Hash) {
        self.out.extend_from_slice(h);
    }
    pub fn opt_str(&mut self, s: Option<&str>) {
        match s {
            Some(s) => {
                self.u8(1);
                self.str(s);
            }
            None => self.u8(0),
        }
    }
    pub fn hashes(&mut self, hs: &[Hash]) {
        self.count(hs.len());
        for h in hs {
            self.hash(h);
        }
    }

    pub fn shape(&mut self, s: &FileShape) {
        self.hash(&s.api);
        self.count(s.provides.len());
        for p in &s.provides {
            let (tag, v) = match &p.what {
                Provides::Export(v) => (0, v),
                Provides::Global(v) => (1, v),
                Provides::Ambient(v) => (2, v),
                Provides::Augments(v) => (3, v),
            };
            self.u8(tag);
            self.str(v);
            self.hash(&p.hash);
            self.refs(&p.deps);
        }
        self.count(s.stars.len());
        for st in &s.stars {
            self.str(st);
        }
        self.refs(&s.uses);
    }

    fn refs(&mut self, rs: &[Ref]) {
        self.count(rs.len());
        for r in rs {
            match r {
                Ref::Export(p, n) => {
                    self.u8(0);
                    self.str(p);
                    self.str(n);
                }
                Ref::All(p) => {
                    self.u8(1);
                    self.str(p);
                }
                Ref::Global(g) => {
                    self.u8(2);
                    self.str(g);
                }
                Ref::Free(g) => {
                    self.u8(3);
                    self.str(g);
                }
                Ref::Ambient(s) => {
                    self.u8(4);
                    self.str(s);
                }
                Ref::LibGlobals => self.u8(5),
                Ref::AllGlobals => self.u8(6),
                Ref::Augs(p) => {
                    self.u8(7);
                    self.str(p);
                }
            }
        }
    }

    pub fn diags(&mut self, ds: &[PathDiag]) {
        self.count(ds.len());
        for d in ds {
            self.diag(d);
        }
    }

    fn diag(&mut self, d: &PathDiag) {
        let x = &d.diag;
        self.u32(x.code);
        self.u8(category_code(x.category));
        self.str(&x.message);
        self.opt_str(d.file.as_deref());
        self.u32(x.span.start);
        self.u32(x.span.end);
        self.count(x.related.len());
        for (r, p) in x.related.iter().zip(&d.related) {
            self.u32(r.code);
            self.u8(category_code(r.category));
            self.str(&r.message);
            self.opt_str(p.as_deref());
            self.u32(r.span.start);
            self.u32(r.span.end);
        }
    }
}

fn category_code(c: Category) -> u8 {
    match c {
        Category::Error => 0,
        Category::Warning => 1,
        Category::Suggestion => 2,
        Category::Message => 3,
    }
}

fn category_of(v: u8) -> Option<Category> {
    Some(match v {
        0 => Category::Error,
        1 => Category::Warning,
        2 => Category::Suggestion,
        3 => Category::Message,
        _ => return None,
    })
}

#[derive(Debug)]
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Reader<'a> {
        Reader { data, pos: 0 }
    }
    pub fn done(&self) -> bool {
        self.pos == self.data.len()
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }
    pub fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    pub fn u32(&mut self) -> Option<u32> {
        let b = self.take(4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    pub fn u64(&mut self) -> Option<u64> {
        let b = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Some(u64::from_le_bytes(a))
    }
    /// A count, bounded by the bytes left (each item takes at least one byte).
    pub fn count(&mut self) -> Option<usize> {
        let n = self.u32()? as usize;
        (n <= self.data.len() - self.pos).then_some(n)
    }
    pub fn bool(&mut self) -> Option<bool> {
        match self.u8()? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }
    pub fn str(&mut self) -> Option<String> {
        let n = self.count()?;
        String::from_utf8(self.take(n)?.to_vec()).ok()
    }
    pub fn hash(&mut self) -> Option<Hash> {
        let b = self.take(32)?;
        let mut h = [0u8; 32];
        h.copy_from_slice(b);
        Some(h)
    }
    pub fn opt_str(&mut self) -> Option<Option<String>> {
        match self.u8()? {
            0 => Some(None),
            1 => Some(Some(self.str()?)),
            _ => None,
        }
    }
    pub fn hashes(&mut self) -> Option<Vec<Hash>> {
        let n = self.count()?;
        (0..n).map(|_| self.hash()).collect()
    }

    pub fn shape(&mut self) -> Option<FileShape> {
        let api = self.hash()?;
        let n = self.count()?;
        let mut provides = Vec::with_capacity(n);
        for _ in 0..n {
            let tag = self.u8()?;
            let v = self.str()?;
            let what = match tag {
                0 => Provides::Export(v),
                1 => Provides::Global(v),
                2 => Provides::Ambient(v),
                3 => Provides::Augments(v),
                _ => return None,
            };
            let hash = self.hash()?;
            let deps = self.refs()?;
            provides.push(Provided { what, hash, deps });
        }
        let n = self.count()?;
        let stars = (0..n).map(|_| self.str()).collect::<Option<Vec<_>>>()?;
        let uses = self.refs()?;
        Some(FileShape {
            api,
            provides,
            stars,
            uses,
        })
    }

    fn refs(&mut self) -> Option<Vec<Ref>> {
        let n = self.count()?;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(match self.u8()? {
                0 => Ref::Export(self.str()?, self.str()?),
                1 => Ref::All(self.str()?),
                2 => Ref::Global(self.str()?),
                3 => Ref::Free(self.str()?),
                4 => Ref::Ambient(self.str()?),
                5 => Ref::LibGlobals,
                6 => Ref::AllGlobals,
                7 => Ref::Augs(self.str()?),
                _ => return None,
            });
        }
        Some(out)
    }

    pub fn diags(&mut self) -> Option<Vec<PathDiag>> {
        let n = self.count()?;
        (0..n).map(|_| self.diag()).collect()
    }

    fn diag(&mut self) -> Option<PathDiag> {
        let code = self.u32()?;
        let category = category_of(self.u8()?)?;
        let message = self.str()?;
        let file = self.opt_str()?;
        let span = Span {
            start: self.u32()?,
            end: self.u32()?,
        };
        let n = self.count()?;
        let mut related = Vec::with_capacity(n);
        let mut related_paths = Vec::with_capacity(n);
        for _ in 0..n {
            let code = self.u32()?;
            let category = category_of(self.u8()?)?;
            let message = self.str()?;
            let p = self.opt_str()?;
            let span = Span {
                start: self.u32()?,
                end: self.u32()?,
            };
            related.push(RelatedInfo {
                code,
                category,
                message,
                file: FileId(0),
                span,
            });
            related_paths.push(p);
        }
        Some(PathDiag {
            diag: Diagnostic {
                code,
                category,
                message,
                file: FileId(0),
                span,
                related,
            },
            file,
            related: related_paths,
        })
    }
}
