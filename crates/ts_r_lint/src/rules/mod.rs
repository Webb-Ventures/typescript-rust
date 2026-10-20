//! Built-in rules. One module per rule; [`ALL`] lists them.
//!
//! Each module exposes `pub static RULE: RuleDef`. Add new rules to [`ALL`] (see
//! `.agents/skills/add-lint-rule/SKILL.md`).

use serde_json::Value;

use crate::rule::{Category, Preset, Rule, RuleDef, RuleMeta};

mod constructor_super;
mod for_direction;
mod getter_return;
mod no_array_constructor;
mod no_async_promise_executor;
mod no_case_declarations;
mod no_class_assign;
mod no_compare_neg_zero;
mod no_cond_assign;
mod no_console;
mod no_const_assign;
mod no_constant_binary_expression;
mod no_constant_condition;
mod no_control_regex;
mod no_debugger;
mod no_delete_var;
mod no_dupe_args;
mod no_dupe_class_members;
mod no_dupe_else_if;
mod no_dupe_keys;
mod no_duplicate_case;
mod no_empty;
mod no_empty_character_class;
mod no_empty_pattern;
mod no_empty_static_block;
mod no_ex_assign;
mod no_extra_boolean_cast;
mod no_fallthrough;
mod no_func_assign;
mod no_global_assign;
mod no_import_assign;
mod no_invalid_regexp;
mod no_irregular_whitespace;
mod no_loss_of_precision;
mod no_misleading_character_class;
mod no_new_native_nonconstructor;
mod no_nonoctal_decimal_escape;
mod no_obj_calls;
mod no_octal;
mod no_prototype_builtins;
mod no_redeclare;
mod no_regex_spaces;
mod no_self_assign;
mod no_setter_return;
mod no_shadow_restricted_names;
mod no_sparse_arrays;
mod no_this_before_super;
mod no_unassigned_vars;
mod no_undef;
mod no_unexpected_multiline;
mod no_unreachable;
mod no_unsafe_finally;
mod no_unsafe_negation;
mod no_unsafe_optional_chaining;
mod no_unused_expressions;
mod no_unused_labels;
mod no_unused_private_class_members;
mod no_unused_vars;
mod no_useless_assignment;
mod no_useless_backreference;
mod no_useless_catch;
mod no_useless_constructor;
mod no_useless_escape;
mod no_var;
mod no_with;
mod prefer_const;
mod prefer_rest_params;
mod prefer_spread;
mod preserve_caught_error;
mod require_yield;
mod ts_await_thenable;
mod ts_ban_ts_comment;
mod ts_consistent_type_imports;
mod ts_dot_notation;
mod ts_no_array_constructor;
mod ts_no_array_delete;
mod ts_no_base_to_string;
mod ts_no_confusing_void_expression;
mod ts_no_deprecated;
mod ts_no_dupe_class_members;
mod ts_no_duplicate_enum_values;
mod ts_no_duplicate_type_constituents;
mod ts_no_dynamic_delete;
mod ts_no_empty_object_type;
mod ts_no_explicit_any;
mod ts_no_extra_non_null_assertion;
mod ts_no_extraneous_class;
mod ts_no_floating_promises;
mod ts_no_for_in_array;
mod ts_no_generated_empty_object_type;
mod ts_no_implied_eval;
mod ts_no_invalid_void_type;
mod ts_no_meaningless_void_operator;
mod ts_no_misused_new;
mod ts_no_misused_promises;
mod ts_no_misused_spread;
mod ts_no_mixed_enums;
mod ts_no_namespace;
mod ts_no_non_null_asserted_nullish_coalescing;
mod ts_no_non_null_asserted_optional_chain;
mod ts_no_non_null_assertion;
mod ts_no_redeclare;
mod ts_no_redundant_type_constituents;
mod ts_no_require_imports;
mod ts_no_this_alias;
mod ts_no_unnecessary_boolean_literal_compare;
mod ts_no_unnecessary_condition;
mod ts_no_unnecessary_template_expression;
mod ts_no_unnecessary_type_arguments;
mod ts_no_unnecessary_type_assertion;
mod ts_no_unnecessary_type_constraint;
mod ts_no_unnecessary_type_conversion;
mod ts_no_unnecessary_type_parameters;
mod ts_no_unsafe_argument;
mod ts_no_unsafe_assignment;
mod ts_no_unsafe_call;
mod ts_no_unsafe_declaration_merging;
mod ts_no_unsafe_enum_assignment;
mod ts_no_unsafe_enum_comparison;
mod ts_no_unsafe_function_type;
mod ts_no_unsafe_member_access;
mod ts_no_unsafe_return;
mod ts_no_unsafe_unary_minus;
mod ts_no_unused_expressions;
mod ts_no_unused_vars;
mod ts_no_useless_constructor;
mod ts_no_useless_default_assignment;
mod ts_no_wrapper_object_types;
mod ts_non_nullable_type_assertion_style;
mod ts_only_throw_error;
mod ts_prefer_as_const;
mod ts_prefer_find;
mod ts_prefer_includes;
mod ts_prefer_literal_enum_member;
mod ts_prefer_namespace_keyword;
mod ts_prefer_nullish_coalescing;
mod ts_prefer_optional_chain;
mod ts_prefer_promise_reject_errors;
mod ts_prefer_reduce_type_parameter;
mod ts_prefer_regexp_exec;
mod ts_prefer_return_this_type;
mod ts_prefer_string_starts_ends_with;
mod ts_related_getter_setter_pairs;
mod ts_require_await;
mod ts_restrict_plus_operands;
mod ts_restrict_template_expressions;
mod ts_return_await;
mod ts_switch_exhaustiveness_check;
mod ts_triple_slash_reference;
mod ts_unbound_method;
mod ts_unified_signatures;
mod ts_use_unknown_in_catch_callback_variable;
mod use_isnan;
mod valid_typeof;

/// Everything a rule module usually needs: `use super::prelude::*;`.
#[allow(unused_imports)]
pub(crate) mod prelude {
    pub(crate) use super::{
        ESLINT_REC, FromOptions, NO_PRESET, TS_ESLINT_REC, TS_REC, TS_REC_TC, TS_STRICT,
        TS_STRICT_TC, TS_STYLISTIC_TC, bool_field, create, create_default, meta, obj_opt,
        str_field, str_list,
    };
    pub(crate) use crate::ast_util::*;
    pub(crate) use crate::diagnostic::Fix;
    pub(crate) use crate::rule::{Category, Ctx, Rule, RuleDef};
    pub(crate) use crate::semantic::{
        DeclKind, RefFlags, Resolution, ScopeId, ScopeKind, SymbolFlags, SymbolId,
    };
    pub(crate) use crate::type_util::*;
    pub(crate) use crate::types::{ObjectFlags, Sig, Sym, Ty, TypeFlags, TypeInfo};
    pub(crate) use serde_json::Value;
    pub(crate) use ts_r_ast::{
        AssignOp, BinaryOp, FnFlags, KeywordTypeKind, MethodKind, Modifiers, ModuleKind, Node,
        NodeId, NodeKind, UnaryOp, UpdateOp, VarKind,
    };
    pub(crate) use ts_r_span::Span;
}

/// Every built-in rule.
pub static ALL: &[&RuleDef] = &[
    &constructor_super::RULE,
    &for_direction::RULE,
    &getter_return::RULE,
    &no_array_constructor::RULE,
    &no_async_promise_executor::RULE,
    &no_case_declarations::RULE,
    &no_class_assign::RULE,
    &no_compare_neg_zero::RULE,
    &no_cond_assign::RULE,
    &no_console::RULE,
    &no_const_assign::RULE,
    &no_constant_binary_expression::RULE,
    &no_constant_condition::RULE,
    &no_control_regex::RULE,
    &no_debugger::RULE,
    &no_delete_var::RULE,
    &no_dupe_args::RULE,
    &no_dupe_class_members::RULE,
    &no_dupe_else_if::RULE,
    &no_dupe_keys::RULE,
    &no_duplicate_case::RULE,
    &no_empty::RULE,
    &no_empty_character_class::RULE,
    &no_empty_pattern::RULE,
    &no_empty_static_block::RULE,
    &no_ex_assign::RULE,
    &no_extra_boolean_cast::RULE,
    &no_fallthrough::RULE,
    &no_func_assign::RULE,
    &no_global_assign::RULE,
    &no_import_assign::RULE,
    &no_invalid_regexp::RULE,
    &no_irregular_whitespace::RULE,
    &no_loss_of_precision::RULE,
    &no_misleading_character_class::RULE,
    &no_new_native_nonconstructor::RULE,
    &no_nonoctal_decimal_escape::RULE,
    &no_obj_calls::RULE,
    &no_octal::RULE,
    &no_prototype_builtins::RULE,
    &no_redeclare::RULE,
    &no_regex_spaces::RULE,
    &no_self_assign::RULE,
    &no_setter_return::RULE,
    &no_shadow_restricted_names::RULE,
    &no_sparse_arrays::RULE,
    &no_this_before_super::RULE,
    &no_unassigned_vars::RULE,
    &no_undef::RULE,
    &no_unexpected_multiline::RULE,
    &no_unreachable::RULE,
    &no_unsafe_finally::RULE,
    &no_unsafe_negation::RULE,
    &no_unsafe_optional_chaining::RULE,
    &no_unused_expressions::RULE,
    &no_unused_labels::RULE,
    &no_unused_private_class_members::RULE,
    &no_unused_vars::RULE,
    &no_useless_assignment::RULE,
    &no_useless_backreference::RULE,
    &no_useless_catch::RULE,
    &no_useless_constructor::RULE,
    &no_useless_escape::RULE,
    &no_var::RULE,
    &no_with::RULE,
    &prefer_const::RULE,
    &prefer_rest_params::RULE,
    &prefer_spread::RULE,
    &preserve_caught_error::RULE,
    &require_yield::RULE,
    &ts_await_thenable::RULE,
    &ts_ban_ts_comment::RULE,
    &ts_consistent_type_imports::RULE,
    &ts_dot_notation::RULE,
    &ts_no_array_constructor::RULE,
    &ts_no_base_to_string::RULE,
    &ts_no_array_delete::RULE,
    &ts_no_confusing_void_expression::RULE,
    &ts_no_deprecated::RULE,
    &ts_no_dupe_class_members::RULE,
    &ts_no_duplicate_enum_values::RULE,
    &ts_no_duplicate_type_constituents::RULE,
    &ts_no_dynamic_delete::RULE,
    &ts_no_empty_object_type::RULE,
    &ts_no_explicit_any::RULE,
    &ts_no_extra_non_null_assertion::RULE,
    &ts_no_extraneous_class::RULE,
    &ts_no_floating_promises::RULE,
    &ts_no_for_in_array::RULE,
    &ts_no_generated_empty_object_type::RULE,
    &ts_no_implied_eval::RULE,
    &ts_no_invalid_void_type::RULE,
    &ts_no_meaningless_void_operator::RULE,
    &ts_no_misused_new::RULE,
    &ts_no_misused_promises::RULE,
    &ts_no_misused_spread::RULE,
    &ts_no_mixed_enums::RULE,
    &ts_no_namespace::RULE,
    &ts_no_non_null_asserted_nullish_coalescing::RULE,
    &ts_no_non_null_asserted_optional_chain::RULE,
    &ts_no_non_null_assertion::RULE,
    &ts_no_redeclare::RULE,
    &ts_no_redundant_type_constituents::RULE,
    &ts_no_require_imports::RULE,
    &ts_no_this_alias::RULE,
    &ts_no_unnecessary_boolean_literal_compare::RULE,
    &ts_no_unnecessary_condition::RULE,
    &ts_no_unnecessary_type_assertion::RULE,
    &ts_no_unnecessary_template_expression::RULE,
    &ts_no_unnecessary_type_arguments::RULE,
    &ts_no_unnecessary_type_constraint::RULE,
    &ts_no_unnecessary_type_conversion::RULE,
    &ts_no_unsafe_argument::RULE,
    &ts_no_unsafe_assignment::RULE,
    &ts_no_unsafe_call::RULE,
    &ts_no_unsafe_declaration_merging::RULE,
    &ts_no_unnecessary_type_parameters::RULE,
    &ts_no_unsafe_enum_assignment::RULE,
    &ts_no_unsafe_enum_comparison::RULE,
    &ts_no_unsafe_function_type::RULE,
    &ts_no_unsafe_member_access::RULE,
    &ts_no_unsafe_return::RULE,
    &ts_no_unsafe_unary_minus::RULE,
    &ts_no_unused_expressions::RULE,
    &ts_no_unused_vars::RULE,
    &ts_no_useless_constructor::RULE,
    &ts_no_useless_default_assignment::RULE,
    &ts_no_wrapper_object_types::RULE,
    &ts_non_nullable_type_assertion_style::RULE,
    &ts_only_throw_error::RULE,
    &ts_prefer_as_const::RULE,
    &ts_prefer_find::RULE,
    &ts_prefer_includes::RULE,
    &ts_prefer_literal_enum_member::RULE,
    &ts_prefer_namespace_keyword::RULE,
    &ts_prefer_nullish_coalescing::RULE,
    &ts_prefer_optional_chain::RULE,
    &ts_prefer_promise_reject_errors::RULE,
    &ts_require_await::RULE,
    &ts_restrict_plus_operands::RULE,
    &ts_restrict_template_expressions::RULE,
    &ts_switch_exhaustiveness_check::RULE,
    &ts_prefer_reduce_type_parameter::RULE,
    &ts_prefer_regexp_exec::RULE,
    &ts_prefer_return_this_type::RULE,
    &ts_prefer_string_starts_ends_with::RULE,
    &ts_related_getter_setter_pairs::RULE,
    &ts_return_await::RULE,
    &ts_triple_slash_reference::RULE,
    &ts_unbound_method::RULE,
    &ts_unified_signatures::RULE,
    &ts_use_unknown_in_catch_callback_variable::RULE,
    &use_isnan::RULE,
    &valid_typeof::RULE,
];

pub(crate) const ESLINT_REC: &[Preset] = &[Preset::EslintRecommended];
#[allow(dead_code)]
pub(crate) const TS_REC: &[Preset] = &[Preset::TsRecommended, Preset::TsStrict];
#[allow(dead_code)]
pub(crate) const TS_STRICT: &[Preset] = &[Preset::TsStrict];
#[allow(dead_code)]
pub(crate) const TS_ESLINT_REC: &[Preset] = &[Preset::TsEslintRecommended];
#[allow(dead_code)]
pub(crate) const NO_PRESET: &[Preset] = &[];
/// `recommended-type-checked` (and so `strict-type-checked`).
#[allow(dead_code)]
pub(crate) const TS_REC_TC: &[Preset] = &[
    Preset::TsRecommendedTypeChecked,
    Preset::TsStrictTypeChecked,
];
/// `strict-type-checked` only.
#[allow(dead_code)]
pub(crate) const TS_STRICT_TC: &[Preset] = &[Preset::TsStrictTypeChecked];
/// `stylistic-type-checked`.
#[allow(dead_code)]
pub(crate) const TS_STYLISTIC_TC: &[Preset] = &[Preset::TsStylisticTypeChecked];

/// Starts a [`RuleMeta`].
pub(crate) const fn meta(
    name: &'static str,
    docs: &'static str,
    presets: &'static [Preset],
    category: Category,
) -> RuleMeta {
    RuleMeta {
        name,
        docs,
        fixable: false,
        has_suggestions: false,
        requires_types: false,
        presets,
        category,
    }
}

impl RuleMeta {
    #[allow(dead_code)]
    pub(crate) const fn fixable(mut self) -> RuleMeta {
        self.fixable = true;
        self
    }
    #[allow(dead_code)]
    pub(crate) const fn suggestions(mut self) -> RuleMeta {
        self.has_suggestions = true;
        self
    }
    /// Needs type information.
    #[allow(dead_code)]
    pub(crate) const fn typed(mut self) -> RuleMeta {
        self.requires_types = true;
        self
    }
}

/// Rule options parsed from the config array.
pub(crate) trait FromOptions: Sized {
    fn from_options(opts: &[Value]) -> Result<Self, String>;
}

/// Constructor for rules with options.
#[allow(dead_code)]
pub(crate) fn create<T: Rule + FromOptions + 'static>(
    opts: &[Value],
) -> Result<Box<dyn Rule>, String> {
    Ok(Box::new(T::from_options(opts)?))
}

/// Constructor for rules without options.
pub(crate) fn create_default<T: Rule + Default + 'static>(
    _: &[Value],
) -> Result<Box<dyn Rule>, String> {
    Ok(Box::new(T::default()))
}

/// Option `i` as an object (empty when absent).
#[allow(dead_code)]
pub(crate) fn obj_opt(opts: &[Value], i: usize) -> &serde_json::Map<String, Value> {
    static EMPTY: std::sync::OnceLock<serde_json::Map<String, Value>> = std::sync::OnceLock::new();
    opts.get(i)
        .and_then(Value::as_object)
        .unwrap_or_else(|| EMPTY.get_or_init(serde_json::Map::new))
}

/// A boolean field of an options object.
#[allow(dead_code)]
pub(crate) fn bool_field(o: &serde_json::Map<String, Value>, k: &str, default: bool) -> bool {
    o.get(k).and_then(Value::as_bool).unwrap_or(default)
}

/// A string field of an options object.
#[allow(dead_code)]
pub(crate) fn str_field<'v>(o: &'v serde_json::Map<String, Value>, k: &str) -> Option<&'v str> {
    o.get(k).and_then(Value::as_str)
}

/// A string-array field of an options object (empty when missing).
pub(crate) fn str_list(o: &serde_json::Map<String, Value>, k: &str) -> Vec<String> {
    o.get(k)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_unique() {
        let mut seen = std::collections::HashSet::new();
        for d in ALL {
            assert!(seen.insert(d.meta.name), "duplicate {}", d.meta.name);
        }
    }
}
