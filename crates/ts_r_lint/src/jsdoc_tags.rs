//! What each JSDoc tag may or must carry (a name or namepath, a `{type}`), per mode, after
//! `settings.jsdoc.structuredTags` (eslint-plugin-jsdoc's tag structure). Used by the
//! `valid-types`, `check-types` and `no-undefined-types` rules.

use serde_json::Value;

/// A tag-structure mode: the three parser modes and `permissive`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SMode {
    Jsdoc,
    Typescript,
    Closure,
    Permissive,
}

impl SMode {
    pub const ALL: [SMode; 4] = [
        SMode::Jsdoc,
        SMode::Typescript,
        SMode::Closure,
        SMode::Permissive,
    ];

    pub fn from_setting(s: &str) -> SMode {
        match s {
            "jsdoc" => SMode::Jsdoc,
            "closure" => SMode::Closure,
            "permissive" => SMode::Permissive,
            _ => SMode::Typescript,
        }
    }
}

/// The role of a tag's name position.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub enum Role {
    /// Not specified (a name may appear).
    #[default]
    Unset,
    False,
    True,
    Kind(String),
}

impl Role {
    fn kind(&self) -> &str {
        match self {
            Role::Kind(k) => k,
            _ => "",
        }
    }
    fn truthy(&self) -> bool {
        match self {
            Role::Unset | Role::False => false,
            Role::True => true,
            Role::Kind(k) => !k.is_empty(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct TagInfo {
    pub role: Role,
    /// `None`: unspecified; an array of allowed types counts as `true`.
    pub type_allowed: Option<bool>,
    pub name_required: Option<bool>,
    pub type_required: Option<bool>,
    pub type_or_name_required: Option<bool>,
}

const ND: &str = "namepath-defining";
const NREF: &str = "namepath-referencing";

fn default_info(tag: &str, m: SMode) -> TagInfo {
    use SMode::*;
    let k = |s: &str| Role::Kind(s.to_owned());
    let mut i = TagInfo::default();
    let jsdoc = m == Jsdoc;
    let closure_or_perm = matches!(m, Closure | Permissive);
    match tag {
        "alias" => {
            i.role = k(ND);
            i.type_or_name_required = Some(true);
        }
        "arg" | "argument" | "param" | "prop" | "property" => {
            i.role = k(ND);
            i.name_required = Some(true);
            i.type_allowed = Some(true);
        }
        "augments" => {
            i.role = k(NREF);
            i.type_allowed = Some(true);
            i.type_or_name_required = Some(true);
        }
        "borrows" | "lends" | "memberof" | "memberof!" | "mixes" => {
            i.role = k(NREF);
            i.type_or_name_required = Some(true);
        }
        "callback" | "event" => {
            i.role = k(ND);
            i.name_required = Some(true);
        }
        "class" | "const" | "constant" | "constructor" | "member" | "namespace" | "var" => {
            i.role = k(ND);
            i.type_allowed = Some(true);
        }
        "constructs" | "function" | "mixin" => {
            i.role = k(ND);
            i.name_required = Some(false);
            i.type_allowed = Some(false);
        }
        "define" => i.type_required = Some(m == Closure),
        "emits" | "fires" | "listens" | "requires" => {
            i.role = k(NREF);
            i.name_required = Some(true);
            i.type_allowed = Some(false);
        }
        "enum" => {
            i.role = k("name-defining");
            i.type_allowed = Some(true);
        }
        "exception" | "modifies" | "return" | "returns" | "throws" | "yield" | "yields" => {
            i.type_allowed = Some(true);
        }
        "export" | "package" | "private" | "protected" | "public" | "static" => {
            i.type_allowed = Some(closure_or_perm);
        }
        "exports" => {
            i.role = k(ND);
            i.name_required = Some(jsdoc);
            i.type_allowed = Some(closure_or_perm);
        }
        "extends" => {
            i.role = k(NREF);
            i.name_required = Some(jsdoc);
            i.type_allowed = Some(!jsdoc);
            i.type_or_name_required = Some(!jsdoc);
        }
        "external" | "host" => {
            i.role = k(ND);
            i.name_required = Some(true);
            i.type_allowed = Some(false);
        }
        "func" | "method" => i.role = k(ND),
        "implements" | "satisfies" | "type" => i.type_required = Some(true),
        "interface" => {
            i.role = if m == Closure { Role::False } else { k(ND) };
            i.type_allowed = Some(false);
        }
        "internal" => i.role = Role::False,
        "link" | "linkcode" | "linkplain" => i.role = k("namepath-or-url-referencing"),
        "module" => {
            i.role = if jsdoc { k(ND) } else { k("text") };
            i.type_allowed = Some(true);
        }
        "name" => {
            i.role = k(ND);
            i.name_required = Some(true);
            i.type_or_name_required = Some(true);
        }
        "see" => i.role = k("text"),
        "suppress" => {
            i.role = if m == Closure {
                Role::False
            } else {
                Role::True
            };
            i.type_required = Some(m == Closure);
        }
        "template" => {
            if jsdoc {
                i.role = k("text");
                i.name_required = Some(false);
                i.type_allowed = Some(false);
            } else {
                i.role = k(NREF);
                i.name_required = Some(true);
                i.type_allowed = Some(true);
            }
        }
        "this" => {
            if jsdoc {
                i.role = k(NREF);
                i.type_or_name_required = Some(true);
                i.type_required = Some(false);
            } else {
                i.role = Role::False;
                i.type_or_name_required = Some(false);
                i.type_required = Some(m != Permissive);
            }
        }
        "tutorial" => {
            i.name_required = Some(true);
            i.type_allowed = Some(false);
        }
        "typedef" => {
            i.role = k("name-defining");
            i.name_required = Some(matches!(m, Jsdoc | Permissive));
            i.type_allowed = Some(true);
            i.type_or_name_required = Some(m != Typescript);
        }
        _ => {}
    }
    i
}

/// The tag structure of one mode with the user's `structuredTags` applied.
#[derive(Clone, Debug)]
pub struct Structure {
    mode: SMode,
    overrides: Vec<(String, TagInfo)>,
}

/// `settings.jsdoc.structuredTags` as overrides (`name`, `type`, `required`).
fn overrides(settings: &Value) -> Vec<(String, TagInfo)> {
    let Some(tags) = settings.get("structuredTags").and_then(Value::as_object) else {
        return Vec::new();
    };
    tags.iter()
        .map(|(tag, v)| {
            let req = |k: &str| {
                v.get("required")
                    .and_then(Value::as_array)
                    .is_some_and(|r| r.iter().any(|x| x.as_str() == Some(k)))
            };
            let role = match v.get("name") {
                None | Some(Value::Null) => Role::Unset,
                Some(Value::Bool(false)) => Role::False,
                Some(Value::Bool(true)) => Role::True,
                Some(Value::String(s)) => Role::Kind(s.clone()),
                Some(_) => Role::True,
            };
            let type_allowed = match v.get("type") {
                None | Some(Value::Null) => None,
                Some(Value::Bool(b)) => Some(*b),
                Some(_) => Some(true),
            };
            let info = TagInfo {
                role,
                type_allowed,
                name_required: Some(req("name")),
                type_required: Some(req("type")),
                type_or_name_required: Some(req("typeOrNameRequired")),
            };
            (tag.clone(), info)
        })
        .collect()
}

impl Structure {
    pub fn new(mode: SMode, jsdoc_settings: &Value) -> Structure {
        Structure {
            mode,
            overrides: overrides(jsdoc_settings),
        }
    }

    pub fn mode(&self) -> SMode {
        self.mode
    }

    pub fn info(&self, tag: &str) -> TagInfo {
        match self.overrides.iter().find(|(t, _)| t == tag) {
            Some((_, i)) => i.clone(),
            None => default_info(tag, self.mode),
        }
    }

    pub fn must_type(&self, tag: &str) -> bool {
        self.info(tag).type_required == Some(true)
    }

    pub fn might_type(&self, tag: &str) -> bool {
        self.must_type(tag) || self.info(tag).type_allowed != Some(false)
    }

    pub fn might_name_position(&self, tag: &str) -> bool {
        match self.info(tag).role {
            Role::Unset => true,
            r => r.truthy(),
        }
    }

    pub fn might_name_or_namepath(&self, tag: &str) -> bool {
        matches!(self.info(tag).role.kind(), "name-defining" | ND | NREF)
    }

    pub fn might_namepath(&self, tag: &str) -> bool {
        matches!(self.info(tag).role.kind(), ND | NREF)
    }

    pub fn might_name(&self, tag: &str) -> bool {
        self.info(tag).role.kind() == "name-defining"
    }

    pub fn must_name(&self, tag: &str) -> bool {
        self.info(tag).name_required == Some(true)
    }

    pub fn is_name_or_namepath_defining(&self, tag: &str) -> bool {
        matches!(self.info(tag).role.kind(), "name-defining" | ND)
    }

    pub fn is_namepath_referencing(&self, tag: &str) -> bool {
        self.info(tag).role.kind() == NREF
    }

    pub fn is_namepath_or_url_referencing(&self, tag: &str) -> bool {
        self.info(tag).role.kind() == "namepath-or-url-referencing"
    }

    /// `tagMissingRequiredTypeOrNamepath` for a tag with this name, type and name.
    pub fn missing_type_or_namepath(&self, tag: &str, typ: &str, name: &str) -> bool {
        let info = self.info(tag);
        let might_type = self.might_type(tag);
        let has_type = might_type && !typ.is_empty();
        let has_name =
            (self.must_name(tag) || self.might_name_or_namepath(tag)) && !name.is_empty();
        let has_either = (might_type || self.might_name_or_namepath(tag)) && (has_type || has_name);
        info.type_or_name_required == Some(true) && !has_either && !self.must_type(tag)
    }
}

/// A check in the current mode, with what the other modes say (`otherModeMaps`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Answer {
    /// The current mode says yes (or, for "must" checks, every mode does).
    Yes,
    /// Only other modes say yes ("might") / the current mode alone says yes ("must").
    OtherMode,
    No,
}

/// The current mode's structure and the other three.
#[derive(Debug)]
pub struct Modes {
    pub current: Structure,
    pub others: Vec<Structure>,
}

impl Modes {
    pub fn new(mode: SMode, jsdoc_settings: &Value) -> Modes {
        Modes {
            current: Structure::new(mode, jsdoc_settings),
            others: SMode::ALL
                .into_iter()
                .filter(|&m| m != mode)
                .map(|m| Structure::new(m, jsdoc_settings))
                .collect(),
        }
    }

    fn might(&self, f: impl Fn(&Structure) -> bool) -> Answer {
        if f(&self.current) {
            Answer::Yes
        } else if self.others.iter().any(f) {
            Answer::OtherMode
        } else {
            Answer::No
        }
    }

    fn must(&self, f: impl Fn(&Structure) -> bool) -> Answer {
        if !f(&self.current) {
            Answer::No
        } else if self.others.iter().all(f) {
            Answer::Yes
        } else {
            Answer::OtherMode
        }
    }

    pub fn might_name_position(&self, tag: &str) -> Answer {
        self.might(|s| s.might_name_position(tag))
    }

    pub fn might_type(&self, tag: &str) -> Answer {
        self.might(|s| s.might_type(tag))
    }

    pub fn must_name(&self, tag: &str) -> Answer {
        self.must(|s| s.must_name(tag))
    }

    pub fn must_type(&self, tag: &str) -> Answer {
        self.must(|s| s.must_type(tag))
    }

    pub fn missing_type_or_namepath(&self, tag: &str, typ: &str, name: &str) -> Answer {
        self.must(|s| s.missing_type_or_namepath(tag, typ, name))
    }
}
