//! The evaluator: a type annotation of the program, resolved on declarations only (type
//! aliases, interfaces, enums, imports and re-exports), to the validator IR [`G`]. Never asks
//! the checker for a type (record 0007, P4), so the same type gives the same validator in the
//! editor and in a build.

use rustc_hash::FxHashMap;
use ts_r_ast::{
    Ast, KeywordTypeKind, Modifiers, Node, NodeId, NodeList, TypeOperatorKind, UnaryOp,
};
use ts_r_checker::ProgramFiles;
use ts_r_checker::nodes::key_text;

/// A type `parse<T>` can't check at runtime, with the reason (TSR1501).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unsupported {
    /// The type as written at the call site.
    pub ty: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Lit {
    Str(String),
    Num(f64),
    Bool(bool),
    BigInt(String),
}

/// The validator IR.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum G {
    Unknown,
    Str,
    Num,
    Bool,
    BigInt,
    Null,
    Undef,
    Sym,
    Lit(Lit),
    Obj(Box<Obj>),
    Arr(Box<G>),
    Tuple(Box<Tuple>),
    Union(Vec<G>),
    /// A named declaration (`decls[id]`) applied to arguments.
    Ref(usize, Vec<G>),
    /// The enclosing declaration's type parameter.
    Param(usize),
    /// `instanceof` a global class (`Date`, `RegExp`, `URL`, typed arrays).
    Inst(&'static str),
    /// `instanceof` a user class (`Eval::classes[i]`), imported by the generated code.
    Class(usize),
    /// A type with a registered refinement (`GuardOptions::refinements[i]`): its base type,
    /// then the predicate.
    Refine(Box<G>, usize),
    Map(Box<G>, Box<G>),
    Set(Box<G>),
    /// A template literal type: an anchored regular expression and how to show it.
    Template(String, String),
}

#[derive(Clone, Debug, PartialEq, Default)]
pub(crate) struct Obj {
    pub props: Vec<Prop>,
    /// A `string` index signature (or `Record<string, V>`).
    pub index: Option<G>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Prop {
    pub key: String,
    pub ty: G,
    pub optional: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Tuple {
    pub elems: Vec<G>,
    /// Elements before the first optional one.
    pub required: usize,
    pub rest: Option<G>,
}

/// A named declaration the validator calls (hoisted to one function per declaration).
#[derive(Clone, Debug)]
pub(crate) struct Decl {
    pub name: String,
    pub params: usize,
    /// Where it is declared: the file and its first declaration node.
    pub file: usize,
    pub node: NodeId,
    /// The ambient `declare module "x"` block it is declared in, if any.
    pub module: Option<String>,
    /// `None` while its body is being evaluated (a recursive reference).
    pub body: Option<G>,
}

/// Statements names resolve in: a file's top level, or the body of an ambient
/// `declare module "x" { ... }` (whose declarations are all exported).
#[derive(Clone, Copy, Debug)]
struct Block {
    file: usize,
    stmts: NodeList,
    ambient: bool,
}

/// What a name in type position resolves to. The `bool` of `Decl` and `Namespace` is set when
/// the name was reached through a type-only hop (`import type`, `export type`): a class
/// reached that way has no value at the call site.
#[derive(Clone, Debug)]
enum Target {
    /// Declarations of one name in one block (interfaces merge).
    Decl(Block, Vec<NodeId>, bool),
    /// `import * as ns`: the module's block.
    Namespace(Block, bool),
    /// Not declared in the program's sources: a global type by name.
    Global(String),
}

impl Target {
    /// `self`, marked as reached through a type-only hop when `yes`.
    fn type_only_if(self, yes: bool) -> Target {
        match self {
            Target::Decl(b, n, t) => Target::Decl(b, n, t || yes),
            Target::Namespace(b, t) => Target::Namespace(b, t || yes),
            g => g,
        }
    }
}

/// A user class validated with `instanceof` (record 0007, open question 13): where it is
/// declared and how the generated code imports it.
#[derive(Clone, Debug)]
pub(crate) struct ClassRef {
    /// The declared name (`default` for an anonymous default export).
    pub name: String,
    pub file: usize,
    pub node: NodeId,
    /// The ambient `declare module "x"` block it is declared in (then imported from `x`).
    pub module: Option<String>,
    /// The name its module exports it as (`default` for a default export).
    pub export: String,
    /// The JS binding the generated code uses when it imports the class (`$c_<Name>`,
    /// unique per evaluator).
    pub binding: String,
}

/// The error an intersection of a primitive with object types gives: a branded type. Turned
/// into a message naming the type alias (or a generic one) where it is caught.
const BRANDED: &str = "\u{0}branded";

/// The TSR1501 reason for an unregistered branded type alias `name`.
pub(crate) fn branded_reason(name: &str) -> String {
    format!(
        "'{name}' is a branded type; register a predicate for it in '[sound.guard] refinements' (for example {name} = \"./validators#is{name}\")"
    )
}

/// The TSR1501 reason for a branded type written in place (not a named alias).
const BRANDED_INLINE: &str = "a primitive intersected with an object type is a branded type; name it with a type alias and register a predicate for it in '[sound.guard] refinements'";

const MAX_STEPS: u32 = 200_000;
const MAX_NEST: u32 = 200;
const MAX_HOPS: u32 = 32;

type R<T> = Result<T, String>;

#[derive(Debug)]
pub(crate) struct Eval<'p> {
    p: &'p ProgramFiles,
    paths: FxHashMap<&'p str, usize>,
    pub decls: Vec<Decl>,
    ids: FxHashMap<(usize, NodeId), usize>,
    /// User classes reached so far (`G::Class`).
    pub classes: Vec<ClassRef>,
    /// The type names with a registered refinement, in `GuardOptions::refinements` order.
    pub refined: Vec<String>,
    steps: u32,
    nest: u32,
}

/// What [`Eval::rollback`] restores.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Mark {
    decls: usize,
    classes: usize,
}

/// Scope of an evaluation: the file, the ambient module block (if any) names resolve in
/// first, and the type parameters in scope.
#[derive(Clone, Copy)]
struct Scope<'s> {
    file: usize,
    block: Option<Block>,
    params: &'s [String],
}

fn ident(ast: &Ast, n: NodeId) -> Option<&str> {
    match *ast.node(n) {
        Node::Ident { name } => Some(ast.atom(name)),
        Node::StringLit { value } => Some(ast.atom(value)),
        _ => None,
    }
}

fn js_number(v: f64) -> String {
    ts_r_binder::js_number_to_string(v)
}

impl<'p> Eval<'p> {
    pub fn new(p: &'p ProgramFiles) -> Eval<'p> {
        let paths = p
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| (f.path.as_str(), i))
            .collect();
        Eval {
            p,
            paths,
            decls: Vec::new(),
            ids: FxHashMap::default(),
            classes: Vec::new(),
            refined: Vec::new(),
            steps: 0,
            nest: 0,
        }
    }

    fn ast(&self, file: usize) -> &'p Ast {
        &self.p.files[file].ast
    }

    pub fn program(&self) -> &'p ProgramFiles {
        self.p
    }

    /// The type annotation `ty` of `file`.
    pub fn top(&mut self, file: usize, ty: NodeId) -> R<G> {
        self.steps = 0;
        self.nest = 0;
        self.ty(
            Scope {
                file,
                block: None,
                params: &[],
            },
            ty,
        )
    }

    /// An intersection: object types merge. A primitive intersected with object types is a
    /// branded type: `refined` (the alias has a registered refinement) keeps the primitive as
    /// the base and drops the brand members (which need not be checkable), else the
    /// [`BRANDED`] error.
    fn intersection(&mut self, s: Scope<'_>, types: NodeList, refined: bool) -> R<G> {
        let ast = self.ast(s.file);
        let mut objs = Vec::new();
        let mut prims = Vec::new();
        let mut failed = None;
        for &t in ast.list(types).to_vec().iter() {
            let literal = matches!(
                *ast.node(ts_r_checker::nodes::skip_parens(ast, t)),
                Node::TypeLiteral { .. }
            );
            match self.ty(s, t) {
                Ok(g) => match self.expand(&g) {
                    Ok(o) => objs.push(o),
                    Err(e) if matches!(g, G::Ref(..)) && !self.is_primitive(&g) => return Err(e),
                    Err(_) => prims.push(g),
                },
                // A brand member such as `{ readonly [tag]: unique symbol }`.
                Err(e) if literal => {
                    failed.get_or_insert(e);
                }
                Err(e) => return Err(e),
            }
        }
        if !prims.is_empty() && (!objs.is_empty() || failed.is_some()) && !refined {
            return Err(BRANDED.into());
        }
        if prims.len() > 1 {
            return Err("intersections are only supported between object types".into());
        }
        if let Some(p) = prims.pop() {
            return Ok(p);
        }
        if let Some(e) = failed {
            return Err(e);
        }
        let mut merged = Obj::default();
        for o in objs {
            merge(&mut merged, o)?;
        }
        Ok(G::Obj(Box::new(merged)))
    }

    /// Whether reference `g` stands for a type that is not an object type (so it can be the
    /// base of a branded type). A reference still being evaluated counts as an object type.
    fn is_primitive(&self, g: &G) -> bool {
        match self.resolve_union(g) {
            Ok(G::Obj(_) | G::Ref(..)) | Err(_) => false,
            Ok(_) => true,
        }
    }

    /// The declarations and classes so far, to [`Eval::rollback`] to after a failed
    /// evaluation.
    pub fn mark(&self) -> Mark {
        Mark {
            decls: self.decls.len(),
            classes: self.classes.len(),
        }
    }

    pub fn rollback(&mut self, mark: Mark) {
        self.decls.truncate(mark.decls);
        self.ids.retain(|_, id| *id < mark.decls);
        self.classes.truncate(mark.classes);
    }

    fn ty(&mut self, s: Scope<'_>, n: NodeId) -> R<G> {
        self.steps += 1;
        if self.steps > MAX_STEPS {
            return Err("the type is too large to check at runtime".into());
        }
        self.nest += 1;
        let r = if self.nest > MAX_NEST {
            Err("the type is nested too deeply".into())
        } else {
            self.ty_inner(s, n)
        };
        self.nest -= 1;
        match r {
            // `Exclude<"a", "a">`, `keyof {}`, ...: as a written `never`.
            Ok(G::Union(ms)) if ms.is_empty() => Err("no value has type 'never'".into()),
            r => r,
        }
    }

    fn ty_inner(&mut self, s: Scope<'_>, n: NodeId) -> R<G> {
        let ast = self.ast(s.file);
        Ok(match *ast.node(n) {
            Node::ParenType { ty } => return self.ty(s, ty),
            Node::KeywordType { kind } => match kind {
                KeywordTypeKind::Any => return Err("'any' has no shape to check".into()),
                KeywordTypeKind::Never => return Err("no value has type 'never'".into()),
                KeywordTypeKind::Unknown => G::Unknown,
                KeywordTypeKind::Number => G::Num,
                KeywordTypeKind::String => G::Str,
                KeywordTypeKind::Boolean => G::Bool,
                KeywordTypeKind::BigInt => G::BigInt,
                KeywordTypeKind::Symbol => G::Sym,
                KeywordTypeKind::Undefined | KeywordTypeKind::Void => G::Undef,
                KeywordTypeKind::Null => G::Null,
                KeywordTypeKind::Object => {
                    return Err("the 'object' type is not supported yet; describe its shape".into());
                }
                KeywordTypeKind::Intrinsic | KeywordTypeKind::Const => {
                    return Err("it is not a type a value can have".into());
                }
            },
            Node::StringLit { value } => G::Lit(Lit::Str(ast.atom(value).to_owned())),
            Node::NumberLit { value } => G::Lit(Lit::Num(value.get())),
            Node::BoolLit { value } => G::Lit(Lit::Bool(value)),
            Node::BigIntLit { value } => G::Lit(Lit::BigInt(ast.atom(value).to_owned())),
            Node::Unary {
                op: UnaryOp::Minus,
                arg,
            } => match *ast.node(arg) {
                Node::NumberLit { value } => G::Lit(Lit::Num(-value.get())),
                Node::BigIntLit { value } => G::Lit(Lit::BigInt(format!("-{}", ast.atom(value)))),
                _ => return Err("it is not a literal type".into()),
            },
            Node::TemplateLitType { parts } => return self.template(s, parts),
            Node::ArrayType { elem } => G::Arr(Box::new(self.ty(s, elem)?)),
            Node::TupleType { elems } => return self.tuple(s, elems),
            Node::UnionType { types } => {
                let mut out = Vec::new();
                for &t in ast.list(types) {
                    match self.ty(s, t)? {
                        G::Union(ms) => out.extend(ms),
                        g => out.push(g),
                    }
                }
                union(out)
            }
            Node::IntersectionType { types } => {
                return self.intersection(s, types, false).map_err(|e| {
                    if e == BRANDED {
                        BRANDED_INLINE.to_owned()
                    } else {
                        e
                    }
                });
            }
            Node::TypeLiteral { members } => G::Obj(Box::new(self.members(s, members)?)),
            Node::TypeOperator { op, ty } => {
                match op {
                    TypeOperatorKind::Readonly => return self.ty(s, ty),
                    TypeOperatorKind::KeyOf => {
                        let g = self.ty(s, ty)?;
                        let o = self.expand(&g)?;
                        if o.index.is_some() {
                            return Err("'keyof' a type with an index signature is 'string'; write 'string'".into());
                        }
                        union(
                            o.props
                                .into_iter()
                                .map(|p| G::Lit(Lit::Str(p.key)))
                                .collect(),
                        )
                    }
                    TypeOperatorKind::Unique => {
                        return Err("unique symbols can't be recognised at runtime".into());
                    }
                }
            }
            Node::IndexedAccessType { obj, index } => {
                let g = self.ty(s, obj)?;
                let o = self.expand(&g)?;
                let keys = self.ty(s, index)?;
                let keys = match keys {
                    G::Union(ms) => ms,
                    k => vec![k],
                };
                let mut out = Vec::new();
                for k in keys {
                    let G::Lit(Lit::Str(k)) = k else {
                        return Err(
                            "indexed access is only supported with string literal keys".into()
                        );
                    };
                    let p = o
                        .props
                        .iter()
                        .find(|p| p.key == k)
                        .ok_or_else(|| format!("property '{k}' does not exist"))?;
                    out.push(p.ty.clone());
                    if p.optional {
                        out.push(G::Undef);
                    }
                }
                union(out)
            }
            Node::FunctionType { .. } => {
                return Err("function types have no runtime shape".into());
            }
            Node::ConstructorType { .. } => {
                return Err("constructor types have no runtime shape".into());
            }
            Node::TypeQuery { .. } => {
                return Err("'typeof' types need the checker; write the type out".into());
            }
            Node::ConditionalType { .. } | Node::InferType { .. } => {
                return Err("conditional types don't resolve on declarations".into());
            }
            Node::MappedType { .. } => {
                return Err(
                    "mapped types other than Partial, Required, Readonly, Pick, Omit and Record are not supported"
                        .into(),
                );
            }
            Node::ThisType {} => return Err("'this' types depend on the receiver".into()),
            Node::ImportType { .. } => {
                return Err("'import(...)' types are not supported yet; import the type".into());
            }
            Node::TypeRef { name, type_args } => return self.type_ref(s, name, type_args),
            _ => return Err("this kind of type is not supported".into()),
        })
    }

    fn template(&mut self, s: Scope<'_>, parts: NodeList) -> R<G> {
        let ast = self.ast(s.file);
        let items = ast.list(parts).to_vec();
        let mut re = String::from("^");
        let mut shown = String::from("`");
        let mut plain = String::new();
        let mut only_text = true;
        for (i, &part) in items.iter().enumerate() {
            if i % 2 == 0 {
                let Node::TemplateElement { cooked, .. } = *ast.node(part) else {
                    return Err("malformed template literal type".into());
                };
                let t = ast.atom(cooked);
                plain.push_str(t);
                shown.push_str(t);
                re.push_str(&regex_escape(t));
                continue;
            }
            only_text = false;
            let g = self.ty(s, part)?;
            let (r, sh) = template_hole(&g)?;
            re.push_str(&r);
            shown.push_str(&format!("${{{sh}}}"));
        }
        if only_text {
            return Ok(G::Lit(Lit::Str(plain)));
        }
        re.push('$');
        shown.push('`');
        Ok(G::Template(re, shown))
    }

    fn tuple(&mut self, s: Scope<'_>, elems: NodeList) -> R<G> {
        let ast = self.ast(s.file);
        let items = ast.list(elems).to_vec();
        let mut t = Tuple {
            elems: Vec::new(),
            required: 0,
            rest: None,
        };
        let mut seen_optional = false;
        for (i, &e) in items.iter().enumerate() {
            let (ty, optional, rest) = match *ast.node(e) {
                Node::NamedTupleMember {
                    rest, optional, ty, ..
                } => (ty, optional, rest),
                Node::OptionalType { ty } => (ty, true, false),
                Node::RestType { ty } => (ty, false, true),
                _ => (e, false, false),
            };
            let g = self.ty(s, ty)?;
            if rest {
                if i + 1 != items.len() {
                    return Err("rest elements before the end of a tuple are not supported".into());
                }
                match g {
                    G::Arr(el) => t.rest = Some(*el),
                    _ => return Err("a tuple rest element must be an array type".into()),
                }
                continue;
            }
            if optional {
                seen_optional = true;
            } else if seen_optional {
                return Err("a required tuple element after an optional one".into());
            } else {
                t.required += 1;
            }
            t.elems.push(g);
        }
        Ok(G::Tuple(Box::new(t)))
    }

    /// Members of a type literal or interface body.
    fn members(&mut self, s: Scope<'_>, members: NodeList) -> R<Obj> {
        let ast = self.ast(s.file);
        let mut o = Obj::default();
        for &m in ast.list(members).to_vec().iter() {
            match *ast.node(m) {
                Node::PropertySignature {
                    key,
                    optional,
                    type_ann,
                    ..
                } => {
                    let k =
                        key_text(ast, key).ok_or("computed property names are not supported")?;
                    let ty = match type_ann {
                        Some(t) => self.ty(s, t)?,
                        None => return Err(format!("property '{k}' has no type ('any')")),
                    };
                    o.props.retain(|p| p.key != k);
                    o.props.push(Prop {
                        key: k,
                        ty,
                        optional,
                    });
                }
                Node::MethodSignature { key, .. } => {
                    let k = key_text(ast, key).unwrap_or_else(|| "[computed]".into());
                    return Err(format!(
                        "method '{k}' is a function; functions have no runtime shape"
                    ));
                }
                Node::CallSignature { .. } | Node::ConstructSignature { .. } => {
                    return Err("function types have no runtime shape".into());
                }
                Node::IndexSignature {
                    params, type_ann, ..
                } => {
                    let key_ty = ast.list(params).first().and_then(|&p| match *ast.node(p) {
                        Node::Param { type_ann, .. } => type_ann,
                        _ => None,
                    });
                    let key_ok = key_ty.is_some_and(|k| {
                        matches!(
                            *ast.node(k),
                            Node::KeywordType {
                                kind: KeywordTypeKind::String
                            }
                        )
                    });
                    if !key_ok {
                        return Err("only 'string' index signatures are supported".into());
                    }
                    let v = match type_ann {
                        Some(t) => self.ty(s, t)?,
                        None => return Err("an index signature without a type ('any')".into()),
                    };
                    o.index = Some(v);
                }
                _ => return Err("this kind of member is not supported".into()),
            }
        }
        Ok(o)
    }

    fn args(&mut self, s: Scope<'_>, type_args: Option<NodeId>) -> R<Vec<G>> {
        let Some(ta) = type_args else {
            return Ok(Vec::new());
        };
        let ast = self.ast(s.file);
        let Node::TypeArgs { args } = *ast.node(ta) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for &a in ast.list(args).to_vec().iter() {
            out.push(self.ty(s, a)?);
        }
        Ok(out)
    }

    fn type_ref(&mut self, s: Scope<'_>, name: NodeId, type_args: Option<NodeId>) -> R<G> {
        let ast = self.ast(s.file);
        let target = match *ast.node(name) {
            Node::Ident { name: a } => {
                let n = ast.atom(a);
                if let Some(i) = s.params.iter().position(|p| p == n) {
                    if type_args.is_some() {
                        return Err(format!("type parameter '{n}' can't take type arguments"));
                    }
                    return Ok(G::Param(i));
                }
                self.lookup(s, n, 0)
            }
            Node::QualifiedName { left, right } => {
                let (Some(l), Some(r)) = (ident(ast, left), ident(ast, right)) else {
                    return Err("nested qualified names are not supported".into());
                };
                let r = r.to_owned();
                match self.lookup(s, l, 0) {
                    Target::Namespace(b, to) => self.export_of(b, &r, 0).type_only_if(to),
                    Target::Decl(b, nodes, _) => {
                        let f = b.file;
                        let fast = self.ast(f);
                        return match nodes.first().map(|&d| fast.node(d)) {
                            Some(Node::Enum { members, .. }) => {
                                let members = *members;
                                let all = self.enum_members(f, members)?;
                                all.into_iter()
                                    .find(|(k, _)| *k == r)
                                    .map(|(_, v)| G::Lit(v))
                                    .ok_or_else(|| format!("enum member '{l}.{r}' does not exist"))
                            }
                            _ => Err(format!("'{l}.{r}' is not supported")),
                        };
                    }
                    Target::Global(_) => {
                        return Err(format!(
                            "'{l}.{r}' is not declared in the program's sources"
                        ));
                    }
                }
            }
            _ => return Err("this kind of type name is not supported".into()),
        };
        let args = self.args(s, type_args)?;
        self.apply(target, args)
    }

    fn apply(&mut self, target: Target, args: Vec<G>) -> R<G> {
        match target {
            Target::Namespace(..) => Err("a namespace is not a type".into()),
            Target::Global(n) => self.global(&n, args),
            Target::Decl(b, nodes, type_only) => self.decl_ref(b, nodes, args, type_only),
        }
    }

    fn decl_ref(&mut self, b: Block, nodes: Vec<NodeId>, args: Vec<G>, type_only: bool) -> R<G> {
        let f = b.file;
        let ast = self.ast(f);
        let first = nodes[0];
        match *ast.node(first) {
            Node::Enum { members, .. } => {
                let vals = self.enum_members(f, members)?;
                return Ok(union(vals.into_iter().map(|(_, v)| G::Lit(v)).collect()));
            }
            Node::ClassDecl { .. } => return self.class_ref(b, first, &args, type_only),
            Node::TypeAlias { .. } | Node::Interface { .. } => {}
            _ => return Err("this declaration is not a type".into()),
        }
        let id = self.decl_id(b, &nodes)?;
        let (tp, defaults) = self.type_params(f, first);
        if args.len() > tp.len() {
            return Err(format!(
                "'{}' takes {} type arguments",
                self.decls[id].name,
                tp.len()
            ));
        }
        let mut args = args;
        for (i, d) in defaults.iter().enumerate().skip(args.len()) {
            let Some(d) = *d else {
                return Err(format!(
                    "'{}' needs {} type arguments",
                    self.decls[id].name,
                    tp.len()
                ));
            };
            let g = self.ty(
                Scope {
                    file: f,
                    block: b.ambient.then_some(b),
                    params: &tp,
                },
                d,
            )?;
            let g = subst(&g, &args);
            debug_assert!(i == args.len());
            args.push(g);
        }
        Ok(G::Ref(id, args))
    }

    /// A user class: checked with `instanceof` against the class the generated code imports
    /// (open question 13). The class must have a value the validator can import: not
    /// abstract, not generic, not reached through `import type`, not `declare class` outside
    /// a declaration file, and exported from a module.
    fn class_ref(&mut self, b: Block, node: NodeId, args: &[G], type_only: bool) -> R<G> {
        let f = b.file;
        let ast = self.ast(f);
        let Node::ClassDecl {
            modifiers,
            name,
            type_params,
            ..
        } = *ast.node(node)
        else {
            return Err("this declaration is not a type".into());
        };
        let n = name
            .and_then(|n| ident(ast, n))
            .unwrap_or("default")
            .to_owned();
        if modifiers.contains(Modifiers::ABSTRACT) {
            return Err(format!(
                "'{n}' is an abstract class; validate one of its concrete subclasses (or a union of them) instead"
            ));
        }
        if type_params.is_some() || !args.is_empty() {
            return Err(format!(
                "'{n}' is a generic class; 'instanceof' can't check its type arguments"
            ));
        }
        if type_only {
            return Err(format!(
                "class '{n}' is only imported as a type ('import type' or 'export type'), so the validator has no value to check 'instanceof' against; import it as a value"
            ));
        }
        let path = &self.p.files[f].path;
        let dts = [".d.ts", ".d.mts", ".d.cts"]
            .iter()
            .any(|e| path.to_ascii_lowercase().ends_with(e));
        if !b.ambient && modifiers.contains(Modifiers::DECLARE) && !dts {
            return Err(format!(
                "class '{n}' is declared with 'declare class', which has no value at runtime"
            ));
        }
        if !b.ambient && !self.p.files[f].bound.is_module {
            return Err(format!(
                "class '{n}' is a global of a script file; the validator can only import classes from modules"
            ));
        }
        let Some(export) = self.class_export(b, &n, modifiers) else {
            return Err(format!(
                "class '{n}' is not exported from its module; export it so the validator can import it"
            ));
        };
        if let Some(i) = self
            .classes
            .iter()
            .position(|c| c.file == f && c.node == node)
        {
            return Ok(G::Class(i));
        }
        let module = if b.ambient {
            self.block_module_name(b)
        } else {
            None
        };
        let base: String = n
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let mut binding = format!("$c_{base}");
        let mut k = 2;
        while self.classes.iter().any(|c| c.binding == binding) {
            binding = format!("$c_{base}_{k}");
            k += 1;
        }
        self.classes.push(ClassRef {
            name: n,
            file: f,
            node,
            module,
            export,
            binding,
        });
        Ok(G::Class(self.classes.len() - 1))
    }

    /// The name block `b` exports the class declared as `name` (modifiers `m`) as.
    fn class_export(&self, b: Block, name: &str, m: Modifiers) -> Option<String> {
        let shown = |m: Modifiers| {
            if m.contains(Modifiers::DEFAULT) {
                "default".to_owned()
            } else {
                name.to_owned()
            }
        };
        if b.ambient || m.contains(Modifiers::EXPORT) {
            return Some(shown(m));
        }
        let ast = self.ast(b.file);
        for &st in ast.list(b.stmts) {
            match *ast.node(st) {
                Node::ExportNamed {
                    type_only: false,
                    specifiers,
                    source: None,
                    ..
                } => {
                    for &sp in ast.list(specifiers) {
                        if let Node::ExportSpecifier {
                            type_only: false,
                            local,
                            exported,
                        } = *ast.node(sp)
                            && ident(ast, local) == Some(name)
                        {
                            return ident(ast, exported.unwrap_or(local)).map(str::to_owned);
                        }
                    }
                }
                Node::ExportDefaultExpr { expr } if ident(ast, expr) == Some(name) => {
                    return Some("default".into());
                }
                _ => {}
            }
        }
        None
    }

    fn type_params(&self, f: usize, decl: NodeId) -> (Vec<String>, Vec<Option<NodeId>>) {
        let ast = self.ast(f);
        let tp = match *ast.node(decl) {
            Node::TypeAlias { type_params, .. } | Node::Interface { type_params, .. } => {
                type_params
            }
            _ => None,
        };
        let mut names = Vec::new();
        let mut defaults = Vec::new();
        if let Some(tp) = tp
            && let Node::TypeParams { params } = *ast.node(tp)
        {
            for &p in ast.list(params) {
                if let Node::TypeParam { name, default, .. } = *ast.node(p) {
                    names.push(ident(ast, name).unwrap_or("").to_owned());
                    defaults.push(default);
                }
            }
        }
        (names, defaults)
    }

    /// The hoisted declaration for `nodes` (evaluated on first use).
    fn decl_id(&mut self, b: Block, nodes: &[NodeId]) -> R<usize> {
        let f = b.file;
        if let Some(&id) = self.ids.get(&(f, nodes[0])) {
            return Ok(id);
        }
        let ast = self.ast(f);
        let base = match *ast.node(nodes[0]) {
            Node::TypeAlias { name, .. } | Node::Interface { name, .. } => {
                ident(ast, name).unwrap_or("T").to_owned()
            }
            _ => "T".to_owned(),
        };
        let mut name = base.clone();
        let mut k = 2;
        // `e0`, `e1`, ... name the entries (`$is_e0`).
        let entry_like = |n: &str| {
            n.strip_prefix('e')
                .is_some_and(|d| !d.is_empty() && d.bytes().all(|c| c.is_ascii_digit()))
        };
        while entry_like(&name) || self.decls.iter().any(|d| d.name == name) {
            name = format!("{base}_{k}");
            k += 1;
        }
        let (params, _) = self.type_params(f, nodes[0]);
        let id = self.decls.len();
        let module = if b.ambient {
            self.block_module_name(b)
        } else {
            None
        };
        self.decls.push(Decl {
            name,
            params: params.len(),
            file: f,
            node: nodes[0],
            module,
            body: None,
        });
        self.ids.insert((f, nodes[0]), id);
        let s = Scope {
            file: f,
            block: b.ambient.then_some(b),
            params: &params,
        };
        let refinement = self.refined.iter().position(|r| *r == base);
        let body = match *ast.node(nodes[0]) {
            Node::TypeAlias { ty, .. } => {
                match *ast.node(ts_r_checker::nodes::skip_parens(ast, ty)) {
                    Node::IntersectionType { types } => self
                        .intersection(s, types, refinement.is_some())
                        .map_err(|e| {
                            if e == BRANDED {
                                branded_reason(&base)
                            } else {
                                e
                            }
                        })?,
                    _ => self.ty(s, ty)?,
                }
            }
            _ => {
                let mut o = Obj::default();
                for &n in nodes {
                    let Node::Interface { extends, body, .. } = *ast.node(n) else {
                        continue;
                    };
                    for &h in ast.list(extends).to_vec().iter() {
                        let Node::ExprWithTypeArgs { expr, type_args } = *ast.node(h) else {
                            return Err("unsupported 'extends' clause".into());
                        };
                        let target = match *ast.node(expr) {
                            Node::Ident { name } => self.lookup(s, ast.atom(name), 0),
                            _ => {
                                return Err("qualified names in 'extends' are not supported".into());
                            }
                        };
                        let args = self.args(s, type_args)?;
                        let g = self.apply(target, args)?;
                        let base = self.expand(&g)?;
                        for p in base.props {
                            if !o.props.iter().any(|q| q.key == p.key) {
                                o.props.push(p);
                            }
                        }
                        if o.index.is_none() {
                            o.index = base.index;
                        }
                    }
                    let own = self.members(s, body)?;
                    for p in own.props {
                        o.props.retain(|q| q.key != p.key);
                        o.props.push(p);
                    }
                    if own.index.is_some() {
                        o.index = own.index;
                    }
                }
                G::Obj(Box::new(o))
            }
        };
        let body = match refinement {
            Some(r) => G::Refine(Box::new(body), r),
            None => body,
        };
        self.decls[id].body = Some(body);
        Ok(id)
    }

    fn enum_members(&self, f: usize, members: NodeList) -> R<Vec<(String, Lit)>> {
        let ast = self.ast(f);
        let mut out = Vec::new();
        let mut next = Some(0.0f64);
        for &m in ast.list(members) {
            let Node::EnumMember { name, init } = *ast.node(m) else {
                continue;
            };
            let k = key_text(ast, name).unwrap_or_default();
            let v = match init.map(|i| ts_r_checker::nodes::skip_parens(ast, i)) {
                None => match next {
                    Some(n) => Lit::Num(n),
                    None => return Err(format!("enum member '{k}' has no constant value")),
                },
                Some(i) => match *ast.node(i) {
                    Node::NumberLit { value } => Lit::Num(value.get()),
                    Node::StringLit { value } => Lit::Str(ast.atom(value).to_owned()),
                    Node::Unary {
                        op: UnaryOp::Minus,
                        arg,
                    } => match *ast.node(arg) {
                        Node::NumberLit { value } => Lit::Num(-value.get()),
                        _ => return Err(format!("enum member '{k}' is computed")),
                    },
                    _ => return Err(format!("enum member '{k}' is computed")),
                },
            };
            next = match v {
                Lit::Num(n) => Some(n + 1.0),
                _ => None,
            };
            out.push((k, v));
        }
        Ok(out)
    }

    /// The module name of ambient block `b` (`declare module "x"` -> `x`).
    fn block_module_name(&self, b: Block) -> Option<String> {
        let ast = self.ast(b.file);
        let Node::Program { stmts } = *ast.node(ast.root) else {
            return None;
        };
        ast.list(stmts).iter().find_map(|&st| match *ast.node(st) {
            Node::Module {
                name,
                body: Some(body),
                ..
            } if matches!(*ast.node(body), Node::ModuleBlock { stmts } if stmts == b.stmts) => {
                ident(ast, name).map(str::to_owned)
            }
            _ => None,
        })
    }

    /// The ambient `declare module "spec"` block of `file`.
    fn ambient_block(&self, file: usize, spec: &str) -> Option<Block> {
        let ast = self.ast(file);
        let Node::Program { stmts } = *ast.node(ast.root) else {
            return None;
        };
        ast.list(stmts).iter().find_map(|&st| match *ast.node(st) {
            Node::Module {
                name,
                body: Some(body),
                ..
            } if ident(ast, name) == Some(spec) => match *ast.node(body) {
                Node::ModuleBlock { stmts } => Some(Block {
                    file,
                    stmts,
                    ambient: true,
                }),
                _ => None,
            },
            _ => None,
        })
    }

    /// The hoisted declaration `name` of `file` (in its ambient block `module`, if given):
    /// a type alias or interface, evaluated like a reference to it.
    pub fn decl_named(&mut self, file: usize, module: Option<&str>, name: &str) -> R<usize> {
        self.steps = 0;
        self.nest = 0;
        let b = match module {
            Some(m) => self
                .ambient_block(file, m)
                .ok_or_else(|| format!("no 'declare module \"{m}\"' block"))?,
            None => self.root(file),
        };
        match self.lookup_in(b, name, MAX_HOPS) {
            Target::Decl(db, nodes, _) if db.file == file => {
                let ast = self.ast(file);
                match *ast.node(nodes[0]) {
                    Node::TypeAlias { .. } | Node::Interface { .. } => self.decl_id(db, &nodes),
                    _ => Err(format!("'{name}' is not a type alias or interface")),
                }
            }
            _ => Err(format!("'{name}' is not declared in this file")),
        }
    }

    fn root(&self, file: usize) -> Block {
        let ast = self.ast(file);
        let stmts = match *ast.node(ast.root) {
            Node::Program { stmts } => stmts,
            _ => NodeList::EMPTY,
        };
        Block {
            file,
            stmts,
            ambient: false,
        }
    }

    /// A name in type position: the ambient module block first, then the file's top level
    /// (local declarations and imports), else a global.
    fn lookup(&mut self, s: Scope<'_>, name: &str, hops: u32) -> Target {
        if let Some(b) = s.block
            && let t @ (Target::Decl(..) | Target::Namespace(..)) = self.lookup_in(b, name, hops)
        {
            return t;
        }
        let root = self.root(s.file);
        self.lookup_in(root, name, hops)
    }

    fn lookup_in(&mut self, b: Block, name: &str, hops: u32) -> Target {
        if hops > MAX_HOPS {
            return Target::Global(name.to_owned());
        }
        let ast = self.ast(b.file);
        let mut found = Vec::new();
        for &st in ast.list(b.stmts) {
            match *ast.node(st) {
                Node::Interface { name: n, .. } if ident(ast, n) == Some(name) => found.push(st),
                Node::TypeAlias { name: n, .. } | Node::Enum { name: n, .. }
                    if ident(ast, n) == Some(name) =>
                {
                    return Target::Decl(b, vec![st], false);
                }
                Node::ClassDecl { name: Some(n), .. } if ident(ast, n) == Some(name) => {
                    return Target::Decl(b, vec![st], false);
                }
                Node::ImportDecl {
                    type_only: decl_type_only,
                    specifiers,
                    source,
                    ..
                } => {
                    let Some(spec) = ident(ast, source) else {
                        continue;
                    };
                    for &sp in ast.list(specifiers) {
                        let (local, imported, type_only) = match *ast.node(sp) {
                            Node::ImportSpecifier {
                                imported,
                                local,
                                type_only,
                            } => (local, Some(imported.unwrap_or(local)), type_only),
                            Node::ImportDefaultSpecifier { local } => (local, None, false),
                            Node::ImportNamespaceSpecifier { local } => {
                                if ident(ast, local) == Some(name) {
                                    return match self.module(b.file, spec) {
                                        Some(m) => Target::Namespace(m, decl_type_only),
                                        None => Target::Global(name.to_owned()),
                                    };
                                }
                                continue;
                            }
                            _ => continue,
                        };
                        let type_only = type_only || decl_type_only;
                        if ident(ast, local) != Some(name) {
                            continue;
                        }
                        let imported = match imported {
                            Some(i) => ident(ast, i).unwrap_or(name).to_owned(),
                            None => "default".to_owned(),
                        };
                        return match self.module(b.file, spec) {
                            Some(m) => self
                                .export_of(m, &imported, hops + 1)
                                .type_only_if(type_only),
                            None => Target::Global(name.to_owned()),
                        };
                    }
                }
                _ => {}
            }
        }
        if found.is_empty() {
            Target::Global(name.to_owned())
        } else {
            Target::Decl(b, found, false)
        }
    }

    /// The module `spec` names from `file`: the file it resolves to, else an ambient
    /// `declare module "spec"` in any file of the program (first in program order).
    fn module(&self, file: usize, spec: &str) -> Option<Block> {
        let key = (self.p.files[file].path.clone(), spec.to_owned());
        if let Some(Some(r)) = self.p.resolutions.get(&key)
            && let Some(&f) = self.paths.get(r.resolved.as_str())
        {
            return Some(self.root(f));
        }
        for (f, sf) in self.p.files.iter().enumerate() {
            if sf.is_lib || !sf.text.contains(spec) {
                continue;
            }
            let ast = &sf.ast;
            let Node::Program { stmts } = *ast.node(ast.root) else {
                continue;
            };
            for &st in ast.list(stmts) {
                if let Node::Module {
                    name,
                    body: Some(body),
                    ..
                } = *ast.node(st)
                    && matches!(*ast.node(name), Node::StringLit { value } if ast.atom(value) == spec)
                    && let Node::ModuleBlock { stmts } = *ast.node(body)
                {
                    return Some(Block {
                        file: f,
                        stmts,
                        ambient: true,
                    });
                }
            }
        }
        None
    }

    /// What module block `b` exports as `name` (type position).
    fn export_of(&mut self, b: Block, name: &str, hops: u32) -> Target {
        if hops > MAX_HOPS {
            return Target::Global(name.to_owned());
        }
        let ast = self.ast(b.file);
        let exported = |m: Modifiers| b.ambient || m.contains(Modifiers::EXPORT);
        let mut stars = Vec::new();
        for &st in ast.list(b.stmts) {
            match *ast.node(st) {
                Node::Interface {
                    modifiers, name: n, ..
                }
                | Node::TypeAlias {
                    modifiers, name: n, ..
                }
                | Node::Enum {
                    modifiers, name: n, ..
                } if exported(modifiers) && ident(ast, n) == Some(name) => {
                    return self.lookup_in(b, name, hops + 1);
                }
                Node::ClassDecl {
                    modifiers,
                    name: Some(n),
                    ..
                } if exported(modifiers)
                    && !modifiers.contains(Modifiers::DEFAULT)
                    && ident(ast, n) == Some(name) =>
                {
                    return self.lookup_in(b, name, hops + 1);
                }
                // `export default class [C] {}`.
                Node::ClassDecl { modifiers, .. }
                    if name == "default"
                        && modifiers.contains(Modifiers::EXPORT)
                        && modifiers.contains(Modifiers::DEFAULT) =>
                {
                    return Target::Decl(b, vec![st], false);
                }
                // `export default C;`
                Node::ExportDefaultExpr { expr } if name == "default" => {
                    if let Some(local) = ident(ast, expr) {
                        return self.lookup_in(b, local, hops + 1);
                    }
                }
                Node::ExportNamed {
                    type_only,
                    specifiers,
                    source,
                    ..
                } => {
                    for &sp in ast.list(specifiers) {
                        let Node::ExportSpecifier {
                            local,
                            exported,
                            type_only: spec_type_only,
                        } = *ast.node(sp)
                        else {
                            continue;
                        };
                        let out = ident(ast, exported.unwrap_or(local));
                        if out != Some(name) {
                            continue;
                        }
                        let local = ident(ast, local).unwrap_or(name).to_owned();
                        let t = match source.and_then(|s| ident(ast, s)) {
                            Some(spec) => match self.module(b.file, spec) {
                                Some(m) => self.export_of(m, &local, hops + 1),
                                None => Target::Global(name.to_owned()),
                            },
                            None => self.lookup_in(b, &local, hops + 1),
                        };
                        return t.type_only_if(type_only || spec_type_only);
                    }
                }
                Node::ExportAll {
                    type_only,
                    exported: None,
                    source,
                    ..
                } => {
                    if let Some(spec) = ident(ast, source) {
                        stars.push((spec.to_owned(), type_only));
                    }
                }
                _ => {}
            }
        }
        for (spec, type_only) in stars {
            if let Some(m) = self.module(b.file, &spec)
                && let t @ (Target::Decl(..) | Target::Namespace(..)) =
                    self.export_of(m, name, hops + 1)
            {
                return t.type_only_if(type_only);
            }
        }
        Target::Global(name.to_owned())
    }

    /// Global types by name (lib), when no declaration of the program's sources has the name.
    fn global(&mut self, name: &str, args: Vec<G>) -> R<G> {
        let n = args.len();
        let want = |k: usize| -> R<()> {
            if n == k {
                Ok(())
            } else {
                Err(format!("'{name}' takes {k} type arguments"))
            }
        };
        let mut a = args.into_iter();
        Ok(match name {
            "Array" | "ReadonlyArray" => {
                want(1)?;
                G::Arr(Box::new(a.next().unwrap_or(G::Unknown)))
            }
            "Record" => {
                want(2)?;
                let k = a.next().unwrap_or(G::Unknown);
                let v = a.next().unwrap_or(G::Unknown);
                match k {
                    G::Str => G::Obj(Box::new(Obj {
                        props: Vec::new(),
                        index: Some(v),
                    })),
                    k => {
                        let keys = string_literals(&k)
                            .ok_or("'Record' keys must be 'string' or string literals")?;
                        G::Obj(Box::new(Obj {
                            props: keys
                                .into_iter()
                                .map(|key| Prop {
                                    key,
                                    ty: v.clone(),
                                    optional: false,
                                })
                                .collect(),
                            index: None,
                        }))
                    }
                }
            }
            "Partial" | "Required" | "Readonly" => {
                want(1)?;
                let g = a.next().unwrap_or(G::Unknown);
                if name == "Readonly" && matches!(g, G::Arr(_) | G::Tuple(_)) {
                    return Ok(g);
                }
                let mut o = self.expand(&g)?;
                for p in &mut o.props {
                    match name {
                        "Partial" => p.optional = true,
                        "Required" => p.optional = false,
                        _ => {}
                    }
                }
                G::Obj(Box::new(o))
            }
            "Pick" | "Omit" => {
                want(2)?;
                let g = a.next().unwrap_or(G::Unknown);
                let k = a.next().unwrap_or(G::Unknown);
                let keys = string_literals(&self.resolve_union(&k)?)
                    .ok_or_else(|| format!("'{name}' keys must be string literals"))?;
                let mut o = self.expand(&g)?;
                let pick = name == "Pick";
                o.props.retain(|p| keys.contains(&p.key) == pick);
                if pick {
                    o.index = None;
                }
                G::Obj(Box::new(o))
            }
            "NonNullable" => {
                want(1)?;
                let g = self.resolve_union(&a.next().unwrap_or(G::Unknown))?;
                match g {
                    G::Union(ms) => union(
                        ms.into_iter()
                            .filter(|m| !matches!(m, G::Null | G::Undef))
                            .collect(),
                    ),
                    G::Null | G::Undef => return Err("no value has type 'never'".into()),
                    // `true` would accept `null` and `undefined`.
                    G::Unknown => return Err("'NonNullable<unknown>' has no runtime check".into()),
                    g => g,
                }
            }
            "Exclude" | "Extract" => {
                want(2)?;
                let t = self.resolve_union(&a.next().unwrap_or(G::Unknown))?;
                let u = self.resolve_union(&a.next().unwrap_or(G::Unknown))?;
                let ts = match t {
                    G::Union(ms) => ms,
                    g => vec![g],
                };
                let us = match u {
                    G::Union(ms) => ms,
                    g => vec![g],
                };
                if !ts
                    .iter()
                    .chain(&us)
                    .all(|g| matches!(g, G::Lit(_) | G::Null | G::Undef))
                {
                    return Err(format!("'{name}' is only supported over literal unions"));
                }
                let keep = name == "Extract";
                union(ts.into_iter().filter(|t| us.contains(t) == keep).collect())
            }
            "Date" | "RegExp" | "URL" | "Int8Array" | "Uint8Array" | "Uint8ClampedArray"
            | "Int16Array" | "Uint16Array" | "Int32Array" | "Uint32Array" | "Float32Array"
            | "Float64Array" | "BigInt64Array" | "BigUint64Array" | "ArrayBuffer" => {
                want(0)?;
                G::Inst(static_name(name))
            }
            "Map" | "ReadonlyMap" => {
                want(2)?;
                let k = a.next().unwrap_or(G::Unknown);
                let v = a.next().unwrap_or(G::Unknown);
                G::Map(Box::new(k), Box::new(v))
            }
            "Set" | "ReadonlySet" => {
                want(1)?;
                G::Set(Box::new(a.next().unwrap_or(G::Unknown)))
            }
            "Promise" | "PromiseLike" => {
                return Err("a promise can't be checked synchronously".into());
            }
            "Function" | "CallableFunction" | "NewableFunction" => {
                return Err("function types have no runtime shape".into());
            }
            "String" | "Number" | "Boolean" | "Object" | "Symbol" | "BigInt" => {
                return Err(format!(
                    "'{name}' is a wrapper type; use '{}'",
                    name.to_ascii_lowercase()
                ));
            }
            _ => {
                return Err(format!("'{name}' is not declared in the program's sources"));
            }
        })
    }

    /// `g` with references to type aliases of unions replaced by the union.
    fn resolve_union(&self, g: &G) -> R<G> {
        // At most MAX_NEST hops: `type A = A` (or `A = B`, `B = A`) is a reference forever.
        let mut g = g.clone();
        for _ in 0..=MAX_NEST {
            let G::Ref(id, args) = &g else {
                return Ok(g);
            };
            let body = self.decls[*id]
                .body
                .as_ref()
                .ok_or("the type refers to itself here")?;
            g = subst(body, args);
        }
        Err("the type refers to itself here".into())
    }

    /// `g` as an object type (references expanded one level).
    pub fn expand(&self, g: &G) -> R<Obj> {
        match self.resolve_union(g)? {
            G::Obj(o) => Ok(*o),
            G::Refine(..) => Err(
                "it has a registered refinement, so its shape can't be taken apart; validate the refined type itself"
                    .into(),
            ),
            G::Class(i) => Err(format!(
                "class '{}' is checked with 'instanceof', not by its shape",
                self.classes[i].name
            )),
            _ => Err("it is not an object type".into()),
        }
    }
}

fn static_name(n: &str) -> &'static str {
    const NAMES: &[&str] = &[
        "Date",
        "RegExp",
        "URL",
        "Int8Array",
        "Uint8Array",
        "Uint8ClampedArray",
        "Int16Array",
        "Uint16Array",
        "Int32Array",
        "Uint32Array",
        "Float32Array",
        "Float64Array",
        "BigInt64Array",
        "BigUint64Array",
        "ArrayBuffer",
    ];
    NAMES.iter().find(|x| **x == n).copied().unwrap_or("Object")
}

/// Replaces the enclosing declaration's parameters by `args`.
pub(crate) fn subst(g: &G, args: &[G]) -> G {
    match g {
        G::Param(i) => args.get(*i).cloned().unwrap_or(G::Unknown),
        G::Obj(o) => G::Obj(Box::new(Obj {
            props: o
                .props
                .iter()
                .map(|p| Prop {
                    key: p.key.clone(),
                    ty: subst(&p.ty, args),
                    optional: p.optional,
                })
                .collect(),
            index: o.index.as_ref().map(|i| subst(i, args)),
        })),
        G::Arr(e) => G::Arr(Box::new(subst(e, args))),
        G::Tuple(t) => G::Tuple(Box::new(Tuple {
            elems: t.elems.iter().map(|e| subst(e, args)).collect(),
            required: t.required,
            rest: t.rest.as_ref().map(|r| subst(r, args)),
        })),
        G::Union(ms) => G::Union(ms.iter().map(|m| subst(m, args)).collect()),
        G::Ref(id, a) => G::Ref(*id, a.iter().map(|x| subst(x, args)).collect()),
        G::Map(k, v) => G::Map(Box::new(subst(k, args)), Box::new(subst(v, args))),
        G::Set(v) => G::Set(Box::new(subst(v, args))),
        G::Refine(b, r) => G::Refine(Box::new(subst(b, args)), *r),
        g => g.clone(),
    }
}

/// A union of `ms` without duplicates (`boolean` stays `boolean`).
pub(crate) fn union(ms: Vec<G>) -> G {
    let mut out: Vec<G> = Vec::new();
    for m in ms {
        if !out.contains(&m) {
            out.push(m);
        }
    }
    let t = G::Lit(Lit::Bool(true));
    let f = G::Lit(Lit::Bool(false));
    if out.contains(&t) && out.contains(&f) {
        out.retain(|m| *m != t && *m != f);
        out.push(G::Bool);
    }
    if out.contains(&G::Unknown) {
        return G::Unknown;
    }
    if out.len() == 1 {
        return out.pop().unwrap_or(G::Unknown);
    }
    G::Union(out)
}

fn merge(into: &mut Obj, o: Obj) -> R<()> {
    for p in o.props {
        if let Some(q) = into.props.iter_mut().find(|q| q.key == p.key) {
            if q.ty != p.ty {
                return Err(format!(
                    "property '{}' has different types in the intersection",
                    p.key
                ));
            }
            q.optional &= p.optional;
        } else {
            into.props.push(p);
        }
    }
    if let Some(i) = o.index {
        if into.index.as_ref().is_some_and(|j| *j != i) {
            return Err("index signatures differ in the intersection".into());
        }
        into.index = Some(i);
    }
    Ok(())
}

fn string_literals(g: &G) -> Option<Vec<String>> {
    match g {
        G::Lit(Lit::Str(s)) => Some(vec![s.clone()]),
        G::Union(ms) => ms
            .iter()
            .map(|m| match m {
                G::Lit(Lit::Str(s)) => Some(s.clone()),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

fn regex_escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if "\\^$.|?*+()[]{}/".contains(c) {
            out.push('\\');
        }
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c => out.push(c),
        }
    }
    out
}

/// A `${...}` hole of a template literal type: a regular expression and how to show it.
fn template_hole(g: &G) -> R<(String, String)> {
    Ok(match g {
        G::Str => ("[\\s\\S]*".into(), "string".into()),
        // tsc: a string is a `${number}` when it round-trips through `+s` to a finite number;
        // this accepts decimal, exponent, hex, octal and binary forms without whitespace.
        G::Num => (
            "(?:[+-]?(?:\\d+\\.?\\d*|\\.\\d+)(?:[eE][+-]?\\d+)?|0[xX][0-9a-fA-F]+|0[oO][0-7]+|0[bB][01]+)"
                .into(),
            "number".into(),
        ),
        G::BigInt => ("-?\\d+".into(), "bigint".into()),
        G::Bool => ("(?:true|false)".into(), "boolean".into()),
        G::Null => ("null".into(), "null".into()),
        G::Undef => ("undefined".into(), "undefined".into()),
        G::Lit(l) => {
            let t = match l {
                Lit::Str(s) => s.clone(),
                Lit::Num(n) => js_number(*n),
                Lit::Bool(b) => b.to_string(),
                Lit::BigInt(b) => b.clone(),
            };
            (regex_escape(&t), format!("{t:?}"))
        }
        G::Union(ms) => {
            let mut rs = Vec::new();
            let mut ss = Vec::new();
            for m in ms {
                let (r, s) = template_hole(m)?;
                rs.push(r);
                ss.push(s);
            }
            (format!("(?:{})", rs.join("|")), ss.join(" | "))
        }
        _ => return Err("template literal holes must be primitives or literals".into()),
    })
}
