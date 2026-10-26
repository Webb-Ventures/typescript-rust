//! Built-in rules. One module per rule; [`ALL`] lists them.
//!
//! Each module exposes `pub static RULE: RuleDef`. Add new rules to [`ALL`] (see
//! `.agents/skills/add-lint-rule/SKILL.md`).

use serde_json::Value;

use crate::rule::{Category, Preset, Rule, RuleDef, RuleMeta};

mod constructor_super;
mod default_case;
mod default_case_last;
mod default_param_last;
mod eqeqeq;
mod for_direction;
mod getter_return;
mod grouped_accessor_pairs;
mod guard_for_in;
mod jsdoc_check_access;
mod jsdoc_check_alignment;
mod jsdoc_check_param_names;
mod jsdoc_check_property_names;
mod jsdoc_check_types;
mod jsdoc_check_values;
mod jsdoc_empty_tags;
mod jsdoc_escape_inline_tags;
mod jsdoc_implements_on_classes;
mod jsdoc_multiline_blocks;
mod jsdoc_no_blank_block_descriptions;
mod jsdoc_no_blank_blocks;
mod jsdoc_no_defaults;
mod jsdoc_no_missing_syntax;
mod jsdoc_no_multi_asterisks;
mod jsdoc_no_restricted_syntax;
mod jsdoc_no_types;
mod jsdoc_no_undefined_types;
mod jsdoc_reject_any_type;
mod jsdoc_reject_function_type;
mod jsdoc_require_asterisk_prefix;
mod jsdoc_require_hyphen_before_param_description;
mod jsdoc_require_jsdoc;
mod jsdoc_require_next_description;
mod jsdoc_require_next_type;
mod jsdoc_require_param;
mod jsdoc_require_param_description;
mod jsdoc_require_param_name;
mod jsdoc_require_param_type;
mod jsdoc_require_property;
mod jsdoc_require_property_description;
mod jsdoc_require_property_name;
mod jsdoc_require_property_type;
mod jsdoc_require_returns;
mod jsdoc_require_returns_check;
mod jsdoc_require_returns_description;
mod jsdoc_require_returns_type;
mod jsdoc_require_throws;
mod jsdoc_require_throws_description;
mod jsdoc_require_throws_type;
mod jsdoc_require_yields;
mod jsdoc_require_yields_check;
mod jsdoc_require_yields_description;
mod jsdoc_require_yields_type;
mod jsdoc_tag_lines;
mod jsdoc_valid_types;
mod max_depth;
mod max_nested_callbacks;
mod max_params;
mod no_alert;
mod no_array_constructor;
mod no_async_promise_executor;
mod no_await_in_loop;
mod no_bitwise;
mod no_caller;
mod no_case_declarations;
mod no_class_assign;
mod no_compare_neg_zero;
mod no_cond_assign;
mod no_console;
mod no_const_assign;
mod no_constant_binary_expression;
mod no_constant_condition;
mod no_constructor_return;
mod no_continue;
mod no_control_regex;
mod no_debugger;
mod no_delete_var;
mod no_div_regex;
mod no_dupe_args;
mod no_dupe_class_members;
mod no_dupe_else_if;
mod no_dupe_keys;
mod no_duplicate_case;
mod no_else_return;
mod no_empty;
mod no_empty_character_class;
mod no_empty_function;
mod no_empty_pattern;
mod no_empty_static_block;
mod no_eq_null;
mod no_eval;
mod no_ex_assign;
mod no_extend_native;
mod no_extra_bind;
mod no_extra_boolean_cast;
mod no_fallthrough;
mod no_func_assign;
mod no_global_assign;
mod no_import_assign;
mod no_invalid_regexp;
mod no_irregular_whitespace;
mod no_iterator;
mod no_label_var;
mod no_labels;
mod no_lone_blocks;
mod no_lonely_if;
mod no_loss_of_precision;
mod no_misleading_character_class;
mod no_multi_assign;
mod no_multi_str;
mod no_negated_condition;
mod no_nested_ternary;
mod no_new;
mod no_new_func;
mod no_new_native_nonconstructor;
mod no_new_object;
mod no_new_symbol;
mod no_new_wrappers;
mod no_nonoctal_decimal_escape;
mod no_obj_calls;
mod no_object_constructor;
mod no_octal;
mod no_octal_escape;
mod no_plusplus;
mod no_promise_executor_return;
mod no_proto;
mod no_prototype_builtins;
mod no_redeclare;
mod no_regex_spaces;
mod no_restricted_globals;
mod no_restricted_properties;
mod no_restricted_syntax;
mod no_return_assign;
mod no_script_url;
mod no_self_assign;
mod no_self_compare;
mod no_sequences;
mod no_setter_return;
mod no_shadow_restricted_names;
mod no_sparse_arrays;
mod no_template_curly_in_string;
mod no_this_before_super;
mod no_throw_literal;
mod no_unassigned_vars;
mod no_undef;
mod no_undef_init;
mod no_unexpected_multiline;
mod no_unneeded_ternary;
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
mod no_useless_call;
mod no_useless_catch;
mod no_useless_computed_key;
mod no_useless_concat;
mod no_useless_constructor;
mod no_useless_escape;
mod no_useless_rename;
mod no_var;
mod no_void;
mod no_with;
mod prefer_const;
mod prefer_numeric_literals;
mod prefer_object_has_own;
mod prefer_rest_params;
mod prefer_spread;
mod preserve_caught_error;
mod radix;
mod require_yield;
mod symbol_description;
mod this_binding;
mod ts_adjacent_overload_signatures;
mod ts_array_type;
mod ts_await_thenable;
mod ts_ban_ts_comment;
mod ts_ban_tslint_comment;
mod ts_class_literal_property_style;
mod ts_consistent_generic_constructors;
mod ts_consistent_indexed_object_style;
mod ts_consistent_type_assertions;
mod ts_consistent_type_definitions;
mod ts_consistent_type_imports;
mod ts_dot_notation;
mod ts_no_array_constructor;
mod ts_no_array_delete;
mod ts_no_base_to_string;
mod ts_no_confusing_non_null_assertion;
mod ts_no_confusing_void_expression;
mod ts_no_deprecated;
mod ts_no_dupe_class_members;
mod ts_no_duplicate_enum_values;
mod ts_no_duplicate_type_constituents;
mod ts_no_dynamic_delete;
mod ts_no_empty_function;
mod ts_no_empty_object_type;
mod ts_no_explicit_any;
mod ts_no_extra_non_null_assertion;
mod ts_no_extraneous_class;
mod ts_no_floating_promises;
mod ts_no_for_in_array;
mod ts_no_generated_empty_object_type;
mod ts_no_implied_eval;
mod ts_no_inferrable_types;
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
mod ts_prefer_for_of;
mod ts_prefer_function_type;
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
mod unicorn_catch_error_name;
mod unicorn_empty_brace_spaces;
mod unicorn_error_message;
mod unicorn_escape_case;
mod unicorn_explicit_length_check;
mod unicorn_new_for_builtins;
mod unicorn_no_await_expression_member;
mod unicorn_no_console_spaces;
mod unicorn_no_constant_zero_expression;
mod unicorn_no_document_cookie;
mod unicorn_no_empty_file;
mod unicorn_no_instanceof_builtins;
mod unicorn_no_invalid_fetch_options;
mod unicorn_no_invalid_remove_event_listener;
mod unicorn_no_late_current_target_access;
mod unicorn_no_leading_empty_lines;
mod unicorn_no_lonely_if;
mod unicorn_no_negated_condition;
mod unicorn_no_nested_ternary;
mod unicorn_no_new_array;
mod unicorn_no_null;
mod unicorn_no_object_as_default_parameter;
mod unicorn_no_process_exit;
mod unicorn_no_this_assignment;
mod unicorn_no_this_outside_of_class;
mod unicorn_no_transition_all;
mod unicorn_no_typeof_undefined;
mod unicorn_no_unreadable_array_destructuring;
mod unicorn_no_unreadable_iife;
mod unicorn_no_useless_fallback_in_spread;
mod unicorn_no_useless_switch_case;
mod unicorn_no_xor_as_exponentiation;
mod unicorn_no_zero_fractions;
mod unicorn_number_literal_case;
mod unicorn_numeric_separators_style;
mod unicorn_prefer_add_event_listener;
mod unicorn_prefer_array_flat;
mod unicorn_prefer_array_flat_map;
mod unicorn_prefer_array_index_of;
mod unicorn_prefer_array_some;
mod unicorn_prefer_at;
mod unicorn_prefer_blob_reading_methods;
mod unicorn_prefer_block_statement_over_iife;
mod unicorn_prefer_class_fields;
mod unicorn_prefer_code_point;
mod unicorn_prefer_date_now;
mod unicorn_prefer_module;
mod unicorn_prefer_node_protocol;
mod unicorn_prefer_object_from_entries;
mod unicorn_prefer_optional_catch_binding;
mod unicorn_prefer_prototype_methods;
mod unicorn_prefer_query_selector;
mod unicorn_prefer_reflect_apply;
mod unicorn_prefer_set_size;
mod unicorn_prefer_string_raw;
mod unicorn_prefer_type_error;
mod unicorn_switch_case_braces;
mod unicorn_text_encoding_identifier_case;
mod unicorn_throw_new_error;
mod unicorn_types;
mod unicorn_util;
mod unicorn_util_h4;
mod use_isnan;
mod valid_typeof;
mod yoda;

/// Everything a rule module usually needs: `use super::prelude::*;`.
#[allow(unused_imports)]
pub(crate) mod prelude {
    pub(crate) use super::{
        ESLINT_REC, FromOptions, NO_PRESET, TS_ESLINT_REC, TS_REC, TS_REC_TC, TS_STRICT,
        TS_STRICT_TC, TS_STYLISTIC, TS_STYLISTIC_TC, bool_field, create, create_default,
        max_option, meta, obj_opt, str_field, str_list,
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
    &default_case::RULE,
    &default_case_last::RULE,
    &default_param_last::RULE,
    &eqeqeq::RULE,
    &for_direction::RULE,
    &getter_return::RULE,
    &grouped_accessor_pairs::RULE,
    &guard_for_in::RULE,
    &max_depth::RULE,
    &max_nested_callbacks::RULE,
    &max_params::RULE,
    &no_alert::RULE,
    &no_array_constructor::RULE,
    &no_async_promise_executor::RULE,
    &no_await_in_loop::RULE,
    &no_bitwise::RULE,
    &no_caller::RULE,
    &no_case_declarations::RULE,
    &no_class_assign::RULE,
    &no_compare_neg_zero::RULE,
    &no_cond_assign::RULE,
    &no_console::RULE,
    &no_const_assign::RULE,
    &no_constant_binary_expression::RULE,
    &no_constant_condition::RULE,
    &no_constructor_return::RULE,
    &no_continue::RULE,
    &no_control_regex::RULE,
    &no_debugger::RULE,
    &no_restricted_syntax::RULE,
    &no_delete_var::RULE,
    &no_div_regex::RULE,
    &no_dupe_args::RULE,
    &no_dupe_class_members::RULE,
    &no_dupe_else_if::RULE,
    &no_dupe_keys::RULE,
    &no_duplicate_case::RULE,
    &no_else_return::RULE,
    &no_empty::RULE,
    &no_empty_character_class::RULE,
    &no_empty_function::RULE,
    &no_empty_pattern::RULE,
    &no_empty_static_block::RULE,
    &no_eq_null::RULE,
    &no_eval::RULE,
    &no_ex_assign::RULE,
    &no_extend_native::RULE,
    &no_extra_bind::RULE,
    &no_extra_boolean_cast::RULE,
    &no_fallthrough::RULE,
    &no_func_assign::RULE,
    &no_global_assign::RULE,
    &no_import_assign::RULE,
    &no_invalid_regexp::RULE,
    &no_irregular_whitespace::RULE,
    &no_iterator::RULE,
    &no_label_var::RULE,
    &no_labels::RULE,
    &no_lone_blocks::RULE,
    &no_lonely_if::RULE,
    &no_loss_of_precision::RULE,
    &no_misleading_character_class::RULE,
    &no_multi_assign::RULE,
    &no_multi_str::RULE,
    &no_negated_condition::RULE,
    &no_nested_ternary::RULE,
    &no_new::RULE,
    &no_new_func::RULE,
    &no_new_native_nonconstructor::RULE,
    &no_new_object::RULE,
    &no_new_symbol::RULE,
    &no_new_wrappers::RULE,
    &no_nonoctal_decimal_escape::RULE,
    &no_obj_calls::RULE,
    &no_object_constructor::RULE,
    &no_octal::RULE,
    &no_octal_escape::RULE,
    &no_plusplus::RULE,
    &no_promise_executor_return::RULE,
    &no_proto::RULE,
    &no_prototype_builtins::RULE,
    &no_redeclare::RULE,
    &no_regex_spaces::RULE,
    &no_restricted_globals::RULE,
    &no_restricted_properties::RULE,
    &no_return_assign::RULE,
    &no_script_url::RULE,
    &no_self_assign::RULE,
    &no_self_compare::RULE,
    &no_sequences::RULE,
    &no_setter_return::RULE,
    &no_shadow_restricted_names::RULE,
    &no_sparse_arrays::RULE,
    &no_template_curly_in_string::RULE,
    &no_this_before_super::RULE,
    &no_throw_literal::RULE,
    &no_unassigned_vars::RULE,
    &no_undef::RULE,
    &no_undef_init::RULE,
    &no_unexpected_multiline::RULE,
    &no_unneeded_ternary::RULE,
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
    &no_useless_call::RULE,
    &no_useless_catch::RULE,
    &no_useless_computed_key::RULE,
    &no_useless_concat::RULE,
    &no_useless_constructor::RULE,
    &no_useless_escape::RULE,
    &no_useless_rename::RULE,
    &no_var::RULE,
    &no_void::RULE,
    &no_with::RULE,
    &prefer_const::RULE,
    &prefer_numeric_literals::RULE,
    &prefer_object_has_own::RULE,
    &prefer_rest_params::RULE,
    &prefer_spread::RULE,
    &preserve_caught_error::RULE,
    &radix::RULE,
    &require_yield::RULE,
    &ts_array_type::RULE,
    &ts_adjacent_overload_signatures::RULE,
    &ts_await_thenable::RULE,
    &ts_ban_ts_comment::RULE,
    &ts_consistent_generic_constructors::RULE,
    &ts_consistent_indexed_object_style::RULE,
    &ts_consistent_type_assertions::RULE,
    &ts_ban_tslint_comment::RULE,
    &ts_class_literal_property_style::RULE,
    &ts_consistent_type_definitions::RULE,
    &ts_consistent_type_imports::RULE,
    &ts_dot_notation::RULE,
    &ts_no_array_constructor::RULE,
    &ts_no_base_to_string::RULE,
    &ts_no_array_delete::RULE,
    &ts_no_confusing_non_null_assertion::RULE,
    &ts_no_confusing_void_expression::RULE,
    &ts_no_deprecated::RULE,
    &ts_no_dupe_class_members::RULE,
    &ts_no_duplicate_enum_values::RULE,
    &ts_no_duplicate_type_constituents::RULE,
    &ts_no_dynamic_delete::RULE,
    &ts_no_empty_function::RULE,
    &ts_no_empty_object_type::RULE,
    &ts_no_explicit_any::RULE,
    &ts_no_extra_non_null_assertion::RULE,
    &ts_no_extraneous_class::RULE,
    &ts_no_floating_promises::RULE,
    &ts_no_for_in_array::RULE,
    &ts_no_generated_empty_object_type::RULE,
    &ts_no_implied_eval::RULE,
    &ts_no_inferrable_types::RULE,
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
    &ts_prefer_function_type::RULE,
    &ts_prefer_for_of::RULE,
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
    &symbol_description::RULE,
    &use_isnan::RULE,
    &valid_typeof::RULE,
    &yoda::RULE,
    &jsdoc_check_access::RULE,
    &jsdoc_check_alignment::RULE,
    &jsdoc_check_param_names::RULE,
    &jsdoc_check_property_names::RULE,
    &jsdoc_check_tag_names::RULE,
    &jsdoc_empty_tags::RULE,
    &jsdoc_escape_inline_tags::RULE,
    &jsdoc_implements_on_classes::RULE,
    &jsdoc_multiline_blocks::RULE,
    &jsdoc_no_blank_block_descriptions::RULE,
    &jsdoc_no_blank_blocks::RULE,
    &jsdoc_no_defaults::RULE,
    &jsdoc_no_missing_syntax::RULE,
    &jsdoc_no_multi_asterisks::RULE,
    &jsdoc_no_restricted_syntax::RULE,
    &jsdoc_no_types::RULE,
    &jsdoc_reject_any_type::RULE,
    &jsdoc_reject_function_type::RULE,
    &jsdoc_require_asterisk_prefix::RULE,
    &jsdoc_require_hyphen_before_param_description::RULE,
    &jsdoc_require_jsdoc::RULE,
    &jsdoc_require_next_description::RULE,
    &jsdoc_require_next_type::RULE,
    &jsdoc_require_param::RULE,
    &jsdoc_require_param_description::RULE,
    &jsdoc_require_param_name::RULE,
    &jsdoc_require_param_type::RULE,
    &jsdoc_require_property::RULE,
    &jsdoc_require_property_description::RULE,
    &jsdoc_require_property_name::RULE,
    &jsdoc_require_property_type::RULE,
    &jsdoc_require_returns::RULE,
    &jsdoc_require_returns_check::RULE,
    &jsdoc_require_returns_description::RULE,
    &jsdoc_require_returns_type::RULE,
    &jsdoc_require_throws::RULE,
    &jsdoc_require_throws_description::RULE,
    &jsdoc_require_throws_type::RULE,
    &jsdoc_require_yields::RULE,
    &jsdoc_require_yields_check::RULE,
    &jsdoc_require_yields_description::RULE,
    &jsdoc_require_yields_type::RULE,
    &jsdoc_tag_lines::RULE,
    &jsdoc_valid_types::RULE,
    &jsdoc_check_types::RULE,
    &jsdoc_no_undefined_types::RULE,
    &jsdoc_check_values::RULE,
    // eslint-plugin-unicorn
    &unicorn_catch_error_name::RULE,
    &unicorn_empty_brace_spaces::RULE,
    &unicorn_error_message::RULE,
    &unicorn_escape_case::RULE,
    &unicorn_explicit_length_check::RULE,
    &unicorn_new_for_builtins::RULE,
    &unicorn_no_await_expression_member::RULE,
    &unicorn_no_console_spaces::RULE,
    &unicorn_no_constant_zero_expression::RULE,
    &unicorn_no_document_cookie::RULE,
    &unicorn_no_empty_file::RULE,
    &unicorn_no_instanceof_builtins::RULE,
    &unicorn_no_invalid_fetch_options::RULE,
    &unicorn_no_invalid_remove_event_listener::RULE,
    &unicorn_no_late_current_target_access::RULE,
    &unicorn_no_leading_empty_lines::RULE,
    &unicorn_no_lonely_if::RULE,
    &unicorn_no_negated_condition::RULE,
    &unicorn_no_nested_ternary::RULE,
    &unicorn_no_new_array::RULE,
    &unicorn_no_null::RULE,
    &unicorn_no_object_as_default_parameter::RULE,
    &unicorn_no_process_exit::RULE,
    &unicorn_no_this_assignment::RULE,
    &unicorn_no_this_outside_of_class::RULE,
    &unicorn_no_transition_all::RULE,
    &unicorn_no_typeof_undefined::RULE,
    &unicorn_no_unreadable_array_destructuring::RULE,
    &unicorn_no_unreadable_iife::RULE,
    &unicorn_no_useless_fallback_in_spread::RULE,
    &unicorn_no_useless_switch_case::RULE,
    &unicorn_no_xor_as_exponentiation::RULE,
    &unicorn_no_zero_fractions::RULE,
    &unicorn_number_literal_case::RULE,
    &unicorn_numeric_separators_style::RULE,
    &unicorn_prefer_add_event_listener_options::RULE,
    &unicorn_prefer_array_from_range::RULE,
    &unicorn_prefer_array_slice::RULE,
    &unicorn_prefer_array_last_methods::RULE,
    &unicorn_prefer_blob_reading_methods::RULE,
    &unicorn_prefer_block_statement_over_iife::RULE,
    &unicorn_prefer_at::RULE,
    &unicorn_prefer_class_fields::RULE,
    &unicorn_prefer_date_now::RULE,
    &unicorn_prefer_module::RULE,
    &unicorn_prefer_node_protocol::RULE,
    &unicorn_prefer_number_coercion::RULE,
    &unicorn_prefer_number_properties::RULE,
    &unicorn_prefer_object_destructuring_defaults::RULE,
    &unicorn_prefer_optional_catch_binding::RULE,
    &unicorn_prefer_promise_static_methods::RULE,
    &unicorn_prefer_promise_try::RULE,
    &unicorn_prefer_queue_microtask::RULE,
    &unicorn_prefer_response_static_json::RULE,
    &unicorn_prefer_promise_with_resolvers::RULE,
    &unicorn_prefer_short_escape_sequences::RULE,
    &unicorn_prefer_single_array_predicate::RULE,
    &unicorn_prefer_string_repeat::RULE,
    &unicorn_prefer_structured_clone::RULE,
    &unicorn_prefer_regexp_test::RULE,
    &unicorn_prefer_spread::RULE,
    &unicorn_prefer_string_replace_all::RULE,
    &unicorn_prefer_string_slice::RULE,
    &unicorn_prefer_string_starts_ends_with::RULE,
    &unicorn_prefer_string_trim_start_end::RULE,
    &unicorn_prefer_then_catch::RULE,
    &unicorn_prefer_type_error::RULE,
    &unicorn_switch_case_braces::RULE,
    &unicorn_text_encoding_identifier_case::RULE,
    &unicorn_throw_new_error::RULE,
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
/// `stylistic` (and so `stylistic-type-checked`).
pub(crate) const TS_STYLISTIC: &[Preset] = &[Preset::TsStylistic];
/// The type-checked part of `stylistic-type-checked`.
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

/// The limit of a `max-*` rule: a number, or `{ max }` / `{ maximum }` (`default` when
/// neither is given).
pub(crate) fn max_option(opts: &[Value], default: u64) -> u64 {
    match opts.first() {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(default),
        Some(Value::Object(o)) => o
            .get("max")
            .or_else(|| o.get("maximum"))
            .and_then(Value::as_u64)
            .unwrap_or(default),
        _ => default,
    }
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
