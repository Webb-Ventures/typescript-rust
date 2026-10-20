//! Program levels (history, design record 0005, "Layers").
//!
//! A program's files can be split into *levels*: level 0 (the lib files), then dependencies,
//! then project files. Each level is a [`Layer`]: its files, its symbols (numbered with the
//! level in the top bits of every [`SymId`]), its per-symbol state and a [`TypeStore`] one
//! level above its parent's. A finished layer is shared read-mostly: any number of checkers
//! (other programs, later edits, other projects) build on it, see everything it computed,
//! and add to it only what is made purely of its own and lower levels' entities. Anything
//! that involves a higher level lands in that level (in the store) or in that level's
//! shadow tables (per-symbol state), so a layer never refers to a level above it.
//!
//! What makes this sound is the *closure rule* the caller guarantees when it builds levels:
//! files of a level import only files of the same or lower levels, and no file of a higher
//! level merges into, augments, or declares a global name that a lower level declares or
//! references (see `ts_r_history::levels`). [`Checker::layered`] checks the merge part
//! and refuses levels that break it.

use std::sync::{Arc, OnceLock, RwLock};

use rustc_hash::FxHashMap;
use ts_r_types::{ChunkVec, ResolvedMembers, SymId, TypeId, TypeStore, level_of};

use crate::checker::Checker;
use crate::program::SourceFile;
use crate::symbols::{EnumMemberValue, SymLinks, SymbolTable, SynthSym};

/// Values kept in per-symbol slots, with the level of what they refer to.
pub(crate) trait Leveled {
    fn level(&self, store: &TypeStore) -> u8;
}

impl Leveled for TypeId {
    fn level(&self, _: &TypeStore) -> u8 {
        level_of(self.0)
    }
}

impl Leveled for SymId {
    fn level(&self, _: &TypeStore) -> u8 {
        level_of(self.0)
    }
}

impl Leveled for Box<[TypeId]> {
    fn level(&self, _: &TypeStore) -> u8 {
        self.iter().map(|t| level_of(t.0)).max().unwrap_or(0)
    }
}

impl Leveled for Box<[EnumMemberValue]> {
    fn level(&self, _: &TypeStore) -> u8 {
        self.iter()
            .map(|(s, t)| level_of(s.0).max(t.map_or(0, |t| level_of(t.0))))
            .max()
            .unwrap_or(0)
    }
}

impl Leveled for Box<ResolvedMembers> {
    fn level(&self, store: &TypeStore) -> u8 {
        store.members_level(self)
    }
}

/// Per-symbol values of lower-level symbols that refer to this level, by raw symbol id.
pub(crate) struct Shadow<T> {
    index: RwLock<FxHashMap<u32, u32>>,
    vals: ChunkVec<T>,
}

impl<T> Default for Shadow<T> {
    fn default() -> Self {
        Shadow {
            index: RwLock::new(FxHashMap::default()),
            vals: ChunkVec::new(),
        }
    }
}

impl<T> Shadow<T> {
    fn get(&self, key: u32) -> Option<&T> {
        let i = *self
            .index
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)?;
        Some(self.vals.get(i))
    }

    fn insert(&self, key: u32, v: T) {
        let mut w = self.index.write().unwrap_or_else(|e| e.into_inner());
        w.entry(key).or_insert_with(|| self.vals.push(v));
    }
}

/// Shadow tables of one level, one per [`SymLinks`] slot.
#[derive(Default)]
pub(crate) struct Shadows {
    pub declared: Shadow<TypeId>,
    pub ty: Shadow<TypeId>,
    pub alias: Shadow<SymId>,
    pub bases: Shadow<Box<[TypeId]>>,
    pub enum_members: Shadow<Box<[EnumMemberValue]>>,
    pub raw_members: Shadow<Box<ResolvedMembers>>,
}

/// One level of a program: files, symbols, per-symbol state and types.
pub struct Layer {
    pub(crate) level: u8,
    /// Files of this level (program file ids `file_start..file_end`).
    pub(crate) files: Vec<Arc<SourceFile>>,
    pub(crate) file_start: u32,
    pub(crate) syms: SymbolTable,
    pub(crate) links: Box<[SymLinks]>,
    pub(crate) synth: ChunkVec<SynthSym>,
    pub(crate) shadows: Shadows,
    /// Written names of instantiation queries, kept with their types across workers.
    pub(crate) instantiation_query_names: Shadow<String>,
    pub(crate) store: Arc<TypeStore>,
    pub(crate) parent: Option<Arc<Layer>>,
}

impl std::fmt::Debug for Layer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Layer")
            .field("level", &self.level)
            .field("files", &self.files.len())
            .field("file_start", &self.file_start)
            .finish_non_exhaustive()
    }
}

impl Layer {
    /// This layer's level (0 for the bottom).
    pub fn level(&self) -> u8 {
        self.level
    }

    /// The files of this level.
    pub fn files(&self) -> &[Arc<SourceFile>] {
        &self.files
    }

    /// The first program file id of this level.
    pub fn file_start(&self) -> u32 {
        self.file_start
    }

    /// One past the last program file id of this level.
    pub fn file_end(&self) -> u32 {
        self.file_start + self.files.len() as u32
    }

    /// The level below, if any.
    pub fn parent(&self) -> Option<&Arc<Layer>> {
        self.parent.as_ref()
    }

    /// The type store of this level.
    pub fn store(&self) -> &Arc<TypeStore> {
        &self.store
    }

    /// Approximate heap bytes of this level's tables (types, symbols, per-symbol slots);
    /// files are not counted.
    pub fn heap_bytes(&self) -> usize {
        self.store.heap_bytes()
            + self.links.len() * std::mem::size_of::<SymLinks>()
            + self.synth.heap_bytes()
    }

    /// Number of types created in this level (grows as checks on top of it run).
    pub fn type_count(&self) -> usize {
        self.store.len()
    }

    /// All files from level 0 up to this one, in program order.
    pub fn all_files(&self) -> Vec<Arc<SourceFile>> {
        let mut chain: Vec<&Layer> = Vec::new();
        let mut cur = Some(self);
        while let Some(l) = cur {
            chain.push(l);
            cur = l.parent.as_deref();
        }
        chain
            .iter()
            .rev()
            .flat_map(|l| l.files.iter().cloned())
            .collect()
    }
}

impl Checker<'_> {
    pub(crate) fn instantiation_query_name(&self, t: TypeId) -> Option<&str> {
        self.layers[level_of(t.0) as usize]
            .instantiation_query_names
            .get(t.0)
            .map(String::as_str)
    }

    pub(crate) fn set_instantiation_query_name(&self, t: TypeId, name: String) {
        self.layers[level_of(t.0) as usize]
            .instantiation_query_names
            .insert(t.0, name);
    }
}

/// A per-symbol slot seen from a checker: the symbol's own (shared) slot, plus the shadow
/// tables of the levels above it up to the checker's top level.
pub(crate) struct SlotRef<'c, T> {
    cell: Option<&'c OnceLock<T>>,
    key: u32,
    owner: u8,
    layers: &'c [Arc<Layer>],
    store: &'c TypeStore,
    pick: fn(&Shadows) -> &Shadow<T>,
}

impl<T> Clone for SlotRef<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for SlotRef<'_, T> {}

impl<'c, T: Leveled> SlotRef<'c, T> {
    /// The published value, if any.
    pub fn get(&self) -> Option<&'c T> {
        if let Some(v) = self.cell.and_then(|c| c.get()) {
            return Some(v);
        }
        let pick = self.pick;
        self.layers
            .iter()
            .skip(self.owner as usize + 1)
            .find_map(|l| pick(&l.shadows).get(self.key))
    }

    /// Publishes `v` (first writer wins). A value referring to a level above the symbol's
    /// goes to that level's shadow table instead of the shared slot.
    pub fn set(&self, v: T) -> Result<(), T> {
        let lv = v.level(self.store);
        if lv <= self.owner {
            return match self.cell {
                Some(c) => c.set(v),
                None => Err(v),
            };
        }
        match self.layers.get(lv as usize) {
            Some(l) => {
                (self.pick)(&l.shadows).insert(self.key, v);
                Ok(())
            }
            None => {
                ts_r_types::note_miss(4);
                Err(v)
            }
        }
    }
}

/// [`SymLinks`] of one symbol as seen from a checker (see [`SlotRef`]).
#[derive(Clone, Copy)]
pub(crate) struct LinksRef<'c> {
    pub declared: SlotRef<'c, TypeId>,
    pub ty: SlotRef<'c, TypeId>,
    pub alias: SlotRef<'c, SymId>,
    pub bases: SlotRef<'c, Box<[TypeId]>>,
    pub enum_members: SlotRef<'c, Box<[EnumMemberValue]>>,
    pub raw_members: SlotRef<'c, Box<ResolvedMembers>>,
}

impl<'c> LinksRef<'c> {
    pub(crate) fn new(
        links: &'c SymLinks,
        key: u32,
        owner: u8,
        layers: &'c [Arc<Layer>],
        store: &'c TypeStore,
    ) -> LinksRef<'c> {
        LinksRef {
            declared: SlotRef {
                cell: Some(&links.declared),
                key,
                owner,
                layers,
                store,
                pick: |s| &s.declared,
            },
            ty: SlotRef {
                cell: Some(&links.ty),
                key,
                owner,
                layers,
                store,
                pick: |s| &s.ty,
            },
            alias: SlotRef {
                cell: Some(&links.alias),
                key,
                owner,
                layers,
                store,
                pick: |s| &s.alias,
            },
            bases: SlotRef {
                cell: Some(&links.bases),
                key,
                owner,
                layers,
                store,
                pick: |s| &s.bases,
            },
            enum_members: SlotRef {
                cell: Some(&links.enum_members),
                key,
                owner,
                layers,
                store,
                pick: |s| &s.enum_members,
            },
            raw_members: SlotRef {
                cell: Some(&links.raw_members),
                key,
                owner,
                layers,
                store,
                pick: |s| &s.raw_members,
            },
        }
    }
}
