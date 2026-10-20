//! Grammar errors tsc's checker reports about statements, found while parsing and kept as
//! deferred errors (a file with syntax errors gets none of them). Rules observed in tsc 7
//! (invalid*BreakStatements, invalid*ContinueStatements, classStaticBlock8,
//! invalidContinueInDownlevelAsync, parserReturnStatement1, ambientWithStatements,
//! letDeclarations-scopes):
//!
//! - A `return` outside any function or static block is TS1108.
//! - `break` with no label needs an enclosing loop or `switch`, `continue` an enclosing loop,
//!   and `break L`/`continue L` an enclosing statement labelled `L` (for `continue`, a loop,
//!   possibly through further labels). Function bodies and class static blocks are
//!   boundaries: inside one, a target that is missing or only beyond it is TS1107 "Jump
//!   target cannot cross function boundary." Otherwise a missing target is TS1105 / TS1104
//!   (no label), TS1116 (`break L`) or TS1115 (`continue L`, also when `L` labels something
//!   that is not a loop).
//! - Nothing inside a `with` statement is checked.
//! - A label directly on a `var`/`let`/`const` statement is TS1344 "A label is not allowed
//!   here." (every file is strict in tsc 7).
//! - In an ambient block (a `.d.ts` file, the body of a `declare` namespace or module and
//!   the namespaces inside it) the first statement that is not a declaration (`;` included)
//!   is TS1036 "Statements are not allowed in ambient contexts.", instead of the errors
//!   above for that statement.
//!
//! - Declarators of a `var`/`let`/`const`/`using` statement or `for (;;)` head outside
//!   ambient contexts: a binding pattern without an initializer is TS1182 "A destructuring
//!   declaration must have an initializer.", else a `const`/`using`/`await using` one
//!   TS1155 "'const' declarations must be initialized." (constDeclarations-errors,
//!   declarationWithNoInitializer, usingDeclarations.8). In a `for...in`/`for...of` head a
//!   second declarator is TS1091/TS1188, else an initializer TS1189/TS1190
//!   (parserForInStatement3/4, parserForOfStatement3/4), at the declarator.
//! - A rest parameter before the last parameter is TS1014 (restParameterNotLast).
//! - In an ambient context: a function, method, accessor or constructor body is TS1183
//!   "An implementation cannot be declared in ambient contexts." at the body (also an
//!   accessor body in a type literal or interface), a generator TS1221 at the `*`, and an
//!   initializer TS1039 "Initializers are not allowed in ambient contexts." at the
//!   initializer, except on a `const` (or `readonly` property) without a type: a string or
//!   number literal is fine there, `null`, `true`, objects and the like are TS1254
//!   (constDeclarations-ambient-errors, accessorsInAmbientContext, generatorInAmbientContext*).
//!
//! Errors are at the statement unless said otherwise. The scope stack is pushed and popped around each body, so
//! it stays balanced through speculation and halting.

use ts_r_ast::{
    Atom, HeritageKind, KeywordTypeKind, Modifiers, ModuleKind, Node, NodeId, NodeList, Span,
    UnaryOp, VarKind,
};
use ts_r_diagnostics::{DiagnosticMessage, messages};
use ts_r_lexer::TokenKind as T;

use crate::{Ctx, Parser};

/// Where a statement list is, for [`Parser::check_statement_list`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Container {
    File,
    /// A `namespace N` / `module N` body.
    Namespace,
    /// A `declare module "m"` or `declare global` body.
    ExternalModule,
    /// A block, function body or static block.
    Block,
}

/// What [`Parser::check_class_heritage`] has seen of a class's heritage clauses.
#[derive(Default)]
pub(crate) struct HeritageCheck {
    extends: bool,
    implements: bool,
    reported: bool,
}

/// A statement or body a `break`/`continue` looks through.
#[derive(Clone, Copy)]
pub(crate) enum Jump {
    /// A function body.
    Boundary,
    /// A class static block (a boundary too).
    StaticBlock,
    Loop,
    Switch,
    /// A labelled statement; `is_loop` when it labels a loop.
    Label {
        name: Atom,
        is_loop: bool,
    },
}

impl Parser<'_> {
    /// Runs `f` with `j` on the jump scope stack.
    pub(crate) fn with_jump<R>(&mut self, j: Jump, f: impl FnOnce(&mut Self) -> R) -> R {
        self.jumps.push(j);
        let r = f(self);
        self.jumps.pop();
        r
    }

    /// Runs `f` (a `with` statement's body) with the checks off.
    pub(crate) fn in_with_body<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        self.with_depth += 1;
        let r = f(self);
        self.with_depth -= 1;
        r
    }

    /// The `break`/`continue` statement `stmt` with `label` was just parsed.
    pub(crate) fn check_jump(&mut self, stmt: NodeId, is_break: bool, label: Option<NodeId>) {
        if self.with_depth > 0 {
            return;
        }
        let label = match label.map(|l| self.ast.node(l)) {
            Some(Node::Ident { name }) => Some(*name),
            Some(_) => return,
            None => None,
        };
        let mut crossed = false;
        let mut found = false;
        // `continue L` where `L` labels a statement that is not a loop.
        let mut wrong_kind = false;
        for scope in self.jumps.iter().rev() {
            match (*scope, label) {
                (Jump::Boundary | Jump::StaticBlock, _) => crossed = true,
                (Jump::Loop, None) => found = true,
                (Jump::Switch, None) => found = is_break,
                (Jump::Label { name, is_loop }, Some(l)) if name == l => {
                    if is_break || is_loop {
                        found = true;
                    } else {
                        wrong_kind = true;
                        break;
                    }
                }
                _ => {}
            }
            if found {
                break;
            }
        }
        if found && !crossed {
            return;
        }
        let msg = if crossed {
            messages::JUMP_TARGET_CROSSES_FUNCTION
        } else if wrong_kind || (label.is_some() && !is_break) {
            messages::CONTINUE_TARGET_NOT_LOOP_LABEL
        } else if label.is_some() {
            messages::BREAK_TARGET_NOT_ENCLOSING_LABEL
        } else if is_break {
            messages::BREAK_OUTSIDE_LOOP_OR_SWITCH
        } else {
            messages::CONTINUE_OUTSIDE_LOOP
        };
        let span = self.ast.span(stmt);
        self.deferred_grammar_error_at(msg, span, &[]);
    }

    /// The `return` statement `stmt` was just parsed.
    pub(crate) fn check_return(&mut self, stmt: NodeId) {
        if self.with_depth == 0 && !self.ctx.has(Ctx::IN_FUNCTION) {
            let span = self.ast.span(stmt);
            self.deferred_grammar_error_at(messages::RETURN_OUTSIDE_FUNCTION, span, &[]);
        }
    }

    /// The labelled statement `stmt` with body `body` was just parsed.
    pub(crate) fn check_label(&mut self, stmt: NodeId, body: NodeId) {
        if self.with_depth == 0 && matches!(self.ast.node(body), Node::VarDecl { .. }) {
            let span = self.ast.span(stmt);
            self.deferred_grammar_error_at(messages::LABEL_NOT_ALLOWED_HERE, span, &[]);
        }
    }

    /// The statements of a block that is ambient if the context is; `mark` is the length of
    /// the deferred errors when the block began.
    pub(crate) fn check_ambient_statements(&mut self, stmts: NodeList, mark: usize) {
        if !self.ctx.has(Ctx::AMBIENT) || self.with_depth > 0 {
            return;
        }
        let first = self
            .ast
            .list(stmts)
            .iter()
            .copied()
            .find(|&s| !is_declaration(self.ast.node(s)));
        let Some(stmt) = first else {
            return;
        };
        let span = self.ast.span(stmt);
        // The statement's own errors give way to TS1036 (they are at its start).
        let from = mark.min(self.deferred.len());
        let mut kept = from;
        for i in from..self.deferred.len() {
            if self.deferred[i].span.start != span.start {
                self.deferred.swap(kept, i);
                kept += 1;
            }
        }
        self.deferred.truncate(kept);
        self.deferred_grammar_error_at(messages::STATEMENTS_IN_AMBIENT_CONTEXT, span, &[]);
    }

    /// The declarators `decls` of a `kind` statement or `for (;;)` head.
    pub(crate) fn check_var_initializers(&mut self, kind: VarKind, decls: NodeList) {
        if self.ctx.has(Ctx::AMBIENT) {
            for i in 0..self.ast.list(decls).len() {
                let decl = self.ast.list(decls)[i];
                if let Node::VarDeclarator {
                    type_ann,
                    init: Some(init),
                    ..
                } = *self.ast.node(decl)
                {
                    let const_like = kind == VarKind::Const && type_ann.is_none();
                    self.check_ambient_initializer(init, const_like);
                }
            }
            return;
        }
        let word = match kind {
            VarKind::Var | VarKind::Let => None,
            VarKind::Const => Some("const"),
            VarKind::Using => Some("using"),
            VarKind::AwaitUsing => Some("await using"),
        };
        for i in 0..self.ast.list(decls).len() {
            let decl = self.ast.list(decls)[i];
            let Node::VarDeclarator {
                name, init: None, ..
            } = *self.ast.node(decl)
            else {
                continue;
            };
            let span = self.ast.span(name);
            if matches!(
                self.ast.node(name),
                Node::ObjectPattern { .. } | Node::ArrayPattern { .. }
            ) {
                self.deferred_grammar_error_at(
                    messages::DESTRUCTURING_DECLARATION_NEEDS_INITIALIZER,
                    span,
                    &[],
                );
            } else if let Some(word) = word {
                self.deferred_grammar_error_at(
                    messages::DECLARATIONS_MUST_BE_INITIALIZED,
                    span,
                    &[word],
                );
            }
        }
    }

    /// The left side `left` of a `for...in` (`is_of` false) or `for...of` statement.
    pub(crate) fn check_for_in_of_head(&mut self, left: NodeId, is_of: bool) {
        let Node::VarDecl { decls, .. } = *self.ast.node(left) else {
            return;
        };
        let list = self.ast.list(decls);
        let (msg, at) = if list.len() > 1 {
            let msg = if is_of {
                messages::FOR_OF_SINGLE_DECLARATION
            } else {
                messages::FOR_IN_SINGLE_DECLARATION
            };
            (msg, list[1])
        } else if let Some(&decl) = list.first()
            && matches!(
                self.ast.node(decl),
                Node::VarDeclarator { init: Some(_), .. }
            )
        {
            let msg = if is_of {
                messages::FOR_OF_DECLARATION_INITIALIZER
            } else {
                messages::FOR_IN_DECLARATION_INITIALIZER
            };
            (msg, decl)
        } else {
            return;
        };
        let span = self.ast.span(at);
        self.deferred_grammar_error_at(msg, span, &[]);
    }

    /// The initializer `init` of an ambient variable or property; `const_like`: a `const` or
    /// `readonly` one without a type annotation.
    pub(crate) fn check_ambient_initializer(&mut self, init: NodeId, const_like: bool) {
        let span = self.ast.span(init);
        if !const_like {
            self.deferred_grammar_error_at(messages::AMBIENT_INITIALIZER, span, &[]);
            return;
        }
        let ok = match *self.ast.node(init) {
            Node::StringLit { .. } | Node::NumberLit { .. } | Node::BoolLit { .. } => true,
            Node::TemplateLit { parts } => self.ast.list(parts).len() == 1,
            Node::Unary {
                op: UnaryOp::Minus | UnaryOp::Plus,
                arg,
            } => matches!(self.ast.node(arg), Node::NumberLit { .. }),
            // Maybe a literal enum reference, or not yet known to be fine.
            Node::Ident { .. }
            | Node::Member { .. }
            | Node::Index { .. }
            | Node::Paren { .. }
            | Node::BigIntLit { .. } => true,
            _ => false,
        };
        if !ok {
            self.deferred_grammar_error_at(messages::AMBIENT_CONST_INITIALIZER, span, &[]);
        }
    }

    /// A function, method, accessor or constructor with `body`, and the `*` of a generator.
    pub(crate) fn check_ambient_function(&mut self, star: Option<Span>, body: Option<NodeId>) {
        if !self.ctx.has(Ctx::AMBIENT) {
            // `function* f(): T;`: an overload signature (TS1222).
            if let Some(star) = star
                && body.is_none()
            {
                self.deferred_grammar_error_at(messages::GENERATOR_OVERLOAD, star, &[]);
            }
            return;
        }
        if let Some(star) = star {
            self.deferred_grammar_error_at(messages::AMBIENT_GENERATOR, star, &[]);
        }
        if let Some(body) = body {
            let span = self.ast.span(body);
            self.deferred_grammar_error_at(messages::AMBIENT_IMPLEMENTATION, span, &[]);
        }
    }

    /// A parameter list: the first of TS1014 (rest not last), TS1047 (optional rest), TS1048
    /// (rest with an initializer), TS1015 (`?` and an initializer) and TS1016 (required after
    /// optional), as tsc reports one per list.
    pub(crate) fn check_params(&mut self, params: NodeList) {
        let n = self.ast.list(params).len();
        let mut seen_optional = false;
        let mut first: Option<(DiagnosticMessage, Span)> = None;
        for i in 0..n {
            let p = self.ast.list(params)[i];
            let Node::Param {
                rest,
                pattern,
                optional,
                init,
                ..
            } = *self.ast.node(p)
            else {
                continue;
            };
            if first.is_some() {
                break;
            }
            let span = self.ast.span(p);
            let name = self.ast.span(pattern);
            first = if rest {
                if i + 1 < n {
                    Some((messages::REST_PARAMETER_MUST_BE_LAST, span))
                } else if optional {
                    Some((messages::REST_PARAMETER_OPTIONAL, self.question_after(name)))
                } else if init.is_some() {
                    Some((messages::REST_PARAMETER_INITIALIZER, name))
                } else {
                    None
                }
            } else if optional && init.is_some() {
                Some((messages::QUESTION_MARK_AND_INITIALIZER, name))
            } else if seen_optional && !optional && init.is_none() {
                Some((messages::REQUIRED_AFTER_OPTIONAL, name))
            } else {
                None
            };
            seen_optional |= optional;
        }
        if let Some((m, span)) = first {
            self.deferred_grammar_error_at(m, span, &[]);
        }
    }

    /// The statements of a list in `container`, looking through labels. In a block or under
    /// a label: TS1235 (namespace), TS1234 (ambient module), TS1231 (`export =`), TS1233
    /// (export declarations), TS1258 (`export default x`), TS1232 (imports). TS1035 for a
    /// quoted module name outside an ambient context, and in a namespace TS1319 (`export
    /// default`), TS1194 (export declarations) and TS1147 (`import x = require(...)`).
    pub(crate) fn check_statement_list(&mut self, stmts: NodeList, container: Container) {
        if self.with_depth > 0 {
            return;
        }
        let ambient = self.ctx.has(Ctx::AMBIENT);
        for i in 0..self.ast.list(stmts).len() {
            let stmt = self.ast.list(stmts)[i];
            let mut s = stmt;
            let mut labeled = false;
            // Bounded: each round goes one label deeper into the tree.
            while let Node::Labeled { body, .. } = *self.ast.node(s) {
                s = body;
                labeled = true;
            }
            let in_namespace = container == Container::Namespace && !labeled;
            // Not directly in a file or module body.
            let misplaced = labeled || container == Container::Block;
            let at = self.ast.span(s);
            let error = match *self.ast.node(s) {
                Node::Module {
                    modifiers,
                    kind,
                    name,
                    ..
                } => {
                    let quoted = matches!(self.ast.node(name), Node::StringLit { .. });
                    if misplaced {
                        // A misplaced `module N` gets no TS1540 too.
                        if kind == ModuleKind::Module {
                            let mut n = name;
                            // Bounded: each round goes one level down the qualified name.
                            loop {
                                match *self.ast.node(n) {
                                    Node::QualifiedName { left, right } => {
                                        self.misplaced_module_names.push(self.ast.span(right));
                                        n = left;
                                    }
                                    Node::Ident { .. } => {
                                        self.misplaced_module_names.push(self.ast.span(n));
                                        break;
                                    }
                                    _ => break,
                                }
                            }
                        }
                        let m = if quoted || kind == ModuleKind::Global {
                            messages::AMBIENT_MODULE_NOT_TOP_LEVEL
                        } else {
                            messages::NAMESPACE_NOT_TOP_LEVEL
                        };
                        Some((m, at))
                    } else if quoted && !ambient && !modifiers.contains(Modifiers::DECLARE) {
                        Some((messages::QUOTED_MODULE_NOT_AMBIENT, self.ast.span(name)))
                    } else {
                        None
                    }
                }
                Node::ExportAssignment { .. } if misplaced => {
                    Some((messages::EXPORT_ASSIGNMENT_NOT_TOP_LEVEL, at))
                }
                // JavaScript has no namespaces (even parsed with TypeScript syntax): TS1474
                // and TS1473 there.
                Node::ExportNamed { .. } | Node::ExportAll { .. } if misplaced => {
                    let m = if self.ts && !self.js_ts {
                        messages::EXPORT_NOT_TOP_LEVEL
                    } else {
                        messages::JS_EXPORT_NOT_TOP_LEVEL
                    };
                    Some((m, at))
                }
                Node::ExportDefaultExpr { .. } if misplaced => {
                    Some((messages::DEFAULT_EXPORT_NOT_TOP_LEVEL, at))
                }
                Node::ImportDecl { .. } | Node::ImportEquals { .. } if misplaced => {
                    let m = if self.ts && !self.js_ts {
                        messages::IMPORT_NOT_TOP_LEVEL
                    } else {
                        messages::JS_IMPORT_NOT_TOP_LEVEL
                    };
                    Some((m, at))
                }
                Node::ClassDecl { modifiers, .. } | Node::FunctionDecl { modifiers, .. }
                    if in_namespace && modifiers.contains(Modifiers::DEFAULT) =>
                {
                    self.ast
                        .modifier_tokens_of(s)
                        .iter()
                        .find(|t| t.flag == Modifiers::DEFAULT)
                        .map(|t| (messages::DEFAULT_EXPORT_IN_NAMESPACE, t.span))
                }
                // At a re-export's specifier; `export { a }` only outside ambient contexts.
                Node::ExportNamed { source, .. } if in_namespace => source
                    .map(|src| self.ast.span(src))
                    .or((!ambient).then_some(at))
                    .map(|span| (messages::EXPORT_IN_NAMESPACE, span)),
                Node::ImportEquals { module_ref, .. } if in_namespace => {
                    match *self.ast.node(module_ref) {
                        Node::ExternalModuleRef { expr } => {
                            Some((messages::IMPORT_MODULE_IN_NAMESPACE, self.ast.span(expr)))
                        }
                        _ => None,
                    }
                }
                _ => None,
            };
            if let Some((m, span)) = error {
                self.deferred_grammar_error_at(m, span, &[]);
            }
        }
    }

    /// A constructor's parameters: TS1317 for each rest parameter property.
    pub(crate) fn check_constructor_params(&mut self, params: NodeList) {
        for i in 0..self.ast.list(params).len() {
            let p = self.ast.list(params)[i];
            if let Node::Param {
                modifiers,
                rest: true,
                ..
            } = *self.ast.node(p)
                && modifiers.intersects(
                    Modifiers::PUBLIC
                        | Modifiers::PRIVATE
                        | Modifiers::PROTECTED
                        | Modifiers::READONLY
                        | Modifiers::OVERRIDE,
                )
            {
                let span = self.ast.span(p);
                self.deferred_grammar_error_at(messages::REST_PARAMETER_PROPERTY, span, &[]);
            }
        }
    }

    /// A declaration directly in an `if`, loop or label body (through labels): TS1156 for
    /// `let`, `const`, `using` and `await using` (at the keyword), `type` and `interface` (at
    /// the name), unless it has modifiers. Nothing under a `with` body is checked.
    pub(crate) fn check_embedded_declaration(&mut self, stmt: NodeId) {
        if self.with_depth > 0 {
            return;
        }
        let mut s = stmt;
        // Bounded: each round goes one label deeper into the tree.
        while let Node::Labeled { body, .. } = *self.ast.node(s) {
            s = body;
        }
        let (word, span) = match *self.ast.node(s) {
            // `if (x) export const y`: TS1184 for the modifier instead (the checker's).
            Node::VarDecl { modifiers, .. }
            | Node::TypeAlias { modifiers, .. }
            | Node::Interface { modifiers, .. }
                if !modifiers.is_empty() =>
            {
                return;
            }
            Node::VarDecl { kind, .. } => {
                let word = match kind {
                    VarKind::Var => return,
                    VarKind::Let => "let",
                    VarKind::Const => "const",
                    VarKind::Using => "using",
                    VarKind::AwaitUsing => "await using",
                };
                (word, self.ast.span(s))
            }
            Node::TypeAlias { name, .. } => ("type", self.ast.span(name)),
            Node::Interface { name, .. } => ("interface", self.ast.span(name)),
            _ => return,
        };
        self.deferred_grammar_error_at(messages::DECLARATION_ONLY_IN_BLOCK, span, &[word]);
    }

    /// A class's heritage clause of `kind` (keyword at `keyword`, with `types`): the first of
    /// TS1172 (`extends` again), TS1173 (`extends` after `implements`), TS1174 (more than one
    /// base class) and TS1175 (`implements` again).
    pub(crate) fn check_class_heritage(
        &mut self,
        seen: &mut HeritageCheck,
        kind: HeritageKind,
        keyword: Span,
        types: NodeList,
    ) {
        let error = match kind {
            HeritageKind::Extends => {
                let e = if seen.extends {
                    Some((messages::EXTENDS_CLAUSE_ALREADY_SEEN, keyword))
                } else if seen.implements {
                    Some((messages::EXTENDS_AFTER_IMPLEMENTS, keyword))
                } else if self.ast.list(types).len() > 1 {
                    let second = self.ast.list(types)[1];
                    Some((messages::SINGLE_BASE_CLASS, self.ast.span(second)))
                } else {
                    None
                };
                seen.extends = true;
                e
            }
            HeritageKind::Implements => {
                let e = seen
                    .implements
                    .then_some((messages::IMPLEMENTS_CLAUSE_ALREADY_SEEN, keyword));
                seen.implements = true;
                e
            }
        };
        if let Some((m, span)) = error
            && !seen.reported
        {
            seen.reported = true;
            self.deferred_grammar_error_at(m, span, &[]);
        }
    }

    /// A computed enum member name other than a string or number literal: TS1164, which
    /// tsc's checker reports even in a file with syntax errors.
    pub(crate) fn check_enum_computed_name(&mut self, key: NodeId) {
        let Node::ComputedKey { expr } = *self.ast.node(key) else {
            return;
        };
        if !matches!(
            self.ast.node(expr),
            Node::StringLit { .. } | Node::NumberLit { .. }
        ) {
            let span = self.ast.span(key);
            self.checker_error_at(messages::COMPUTED_NAME_IN_ENUM, span);
        }
    }

    /// An index signature: the first of TS1096 (not exactly one parameter), TS1017 (rest),
    /// TS1018 (accessibility modifier), TS1019 (`?`), TS1020 (initializer), TS1268 (a
    /// keyword parameter type other than string, number and symbol) and TS1021 (no type
    /// annotation, when the parameter's type is certainly valid).
    pub(crate) fn check_index_signature(
        &mut self,
        sig: NodeId,
        params: NodeList,
        type_ann: Option<NodeId>,
    ) {
        let list = self.ast.list(params);
        let error = if list.len() != 1 {
            let at = list.first().map_or(sig, |&p| p);
            Some((messages::INDEX_SIGNATURE_ONE_PARAMETER, self.ast.span(at)))
        } else if let Node::Param {
            modifiers,
            rest,
            pattern,
            optional,
            init,
            type_ann: param_type,
            ..
        } = *self.ast.node(list[0])
        {
            let name = self.ast.span(pattern);
            // The parameter's type comes before the signature's own annotation. Only keyword
            // types are certain here; others (aliases, generics) are the checker's.
            let key_type = param_type.map(|t| match *self.ast.node(t) {
                Node::KeywordType {
                    kind:
                        KeywordTypeKind::String | KeywordTypeKind::Number | KeywordTypeKind::Symbol,
                } => Some(true),
                Node::TemplateLitType { .. } => Some(true),
                Node::KeywordType { .. } => Some(false),
                _ => None,
            });
            if rest {
                Some((messages::INDEX_SIGNATURE_REST, self.ast.span(list[0])))
            } else if modifiers
                .intersects(Modifiers::PUBLIC | Modifiers::PRIVATE | Modifiers::PROTECTED)
            {
                Some((messages::INDEX_SIGNATURE_ACCESSIBILITY, name))
            } else if optional {
                Some((
                    messages::INDEX_SIGNATURE_QUESTION,
                    self.question_after(name),
                ))
            } else if init.is_some() {
                Some((messages::INDEX_SIGNATURE_INITIALIZER, name))
            } else if key_type == Some(Some(false)) {
                Some((messages::INDEX_SIGNATURE_KEY_TYPE, name))
            } else if type_ann.is_none() && key_type == Some(Some(true)) {
                Some((
                    messages::INDEX_SIGNATURE_TYPE_ANNOTATION,
                    self.ast.span(sig),
                ))
            } else {
                None
            }
        } else {
            None
        };
        if let Some((m, span)) = error {
            self.deferred_grammar_error_at(m, span, &[]);
        }
    }

    /// The `?` after a parameter name (the first one after `name`).
    fn question_after(&self, name: Span) -> Span {
        let start = name.end as usize;
        let at = self.src.as_bytes()[start.min(self.src.len())..]
            .iter()
            .position(|&b| b == b'?')
            .map_or(name.end, |i| (start + i) as u32);
        Span::new(at, at + 1)
    }

    /// A definite assignment assertion `!` at `bang`: TS1255 where it is not permitted
    /// (`not_permitted`: ambient, static or abstract), else TS1263 with an initializer, else
    /// TS1264 without a type annotation.
    pub(crate) fn check_definite(
        &mut self,
        bang: Span,
        not_permitted: bool,
        has_type: bool,
        has_init: bool,
    ) {
        let m = if not_permitted {
            messages::DEFINITE_NOT_PERMITTED
        } else if has_init {
            messages::DEFINITE_WITH_INITIALIZER
        } else if !has_type {
            messages::DEFINITE_WITHOUT_TYPE
        } else {
            return;
        };
        self.deferred_grammar_error_at(m, bang, &[]);
    }

    /// `module N` (not `namespace N`): TS1540 at each name in the dotted name, left to right.
    pub(crate) fn check_module_keyword(&mut self, name: NodeId) {
        let mut n = name;
        let mut spans: Vec<Span> = Vec::new();
        // Bounded: each round goes one level down the qualified name.
        loop {
            match *self.ast.node(n) {
                Node::QualifiedName { left, right } => {
                    spans.push(self.ast.span(right));
                    n = left;
                }
                Node::Ident { .. } => {
                    spans.push(self.ast.span(n));
                    break;
                }
                _ => return,
            }
        }
        for &span in spans.iter().rev() {
            self.deferred_grammar_error_at(messages::MODULE_KEYWORD_FOR_NAMESPACE, span, &[]);
        }
    }

    /// Does the statement at the current token (after a label's `:`) start a loop, through
    /// further labels?
    pub(crate) fn at_loop_statement(&mut self) -> bool {
        self.lookahead(|p| {
            // Bounded: each round consumes two tokens or returns.
            loop {
                match p.kind() {
                    T::For | T::While | T::Do => return true,
                    k if k.is_identifier_like() => {
                        p.bump();
                        if !p.eat(T::Colon) {
                            return false;
                        }
                    }
                    _ => return false,
                }
            }
        })
    }
}

/// May this statement appear in an ambient block?
fn is_declaration(node: &Node) -> bool {
    matches!(
        node,
        Node::VarDecl { .. }
            | Node::FunctionDecl { .. }
            | Node::ClassDecl { .. }
            | Node::Interface { .. }
            | Node::TypeAlias { .. }
            | Node::Enum { .. }
            | Node::Module { .. }
            | Node::ImportDecl { .. }
            | Node::ImportEquals { .. }
            | Node::ExportNamed { .. }
            | Node::ExportAll { .. }
            | Node::ExportDefaultExpr { .. }
            | Node::ExportAssignment { .. }
            | Node::NamespaceExport { .. }
            | Node::Directive { .. }
    )
}
