//! Bind-time diagnostic messages. Codes and texts are tsc's (from the synced
//! `diagnosticMessages.json`); they move into `ts_r_diagnostics` once its generated catalog
//! lands.

use ts_r_diagnostics::{Category, DiagnosticMessage};

macro_rules! messages {
    ($($name:ident = $code:literal $cat:ident $text:literal;)*) => {
        $( pub const $name: DiagnosticMessage =
            DiagnosticMessage { code: $code, category: Category::$cat, text: $text }; )*
        /// Every code the binder can report.
        pub const ALL_CODES: &[u32] = &[$($code),*];
    };
}

messages! {
    INVALID_USE_IN_STRICT_MODE = 1100 Error "Invalid use of '{0}' in strict mode.";
    WITH_NOT_ALLOWED_IN_STRICT_MODE = 1101 Error "'with' statements are not allowed in strict mode.";
    DELETE_IDENTIFIER_IN_STRICT_MODE = 1102 Error "'delete' cannot be called on an identifier in strict mode.";
    DUPLICATE_LABEL = 1114 Error "Duplicate label '{0}'.";
    OBJECT_LITERAL_DUPLICATE_PROPERTY = 1117 Error "An object literal cannot have multiple properties with the same name.";
    OBJECT_LITERAL_DUPLICATE_ACCESSOR = 1118 Error "An object literal cannot have multiple get/set accessors with the same name.";
    OBJECT_LITERAL_PROPERTY_AND_ACCESSOR = 1119 Error "An object literal cannot have property and accessor with the same name.";
    INVALID_USE_IN_CLASS = 1210 Error "Code contained in a class is evaluated in JavaScript's strict mode which does not allow this use of '{0}'. For more information, see https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Strict_mode.";
    RESERVED_WORD_IN_STRICT_MODE = 1212 Error "Identifier expected. '{0}' is a reserved word in strict mode.";
    RESERVED_WORD_IN_CLASS = 1213 Error "Identifier expected. '{0}' is a reserved word in strict mode. Class definitions are automatically in strict mode.";
    RESERVED_WORD_IN_MODULE = 1214 Error "Identifier expected. '{0}' is a reserved word in strict mode. Modules are automatically in strict mode.";
    INVALID_USE_IN_MODULE = 1215 Error "Invalid use of '{0}'. Modules are automatically in strict mode.";
    LABEL_NOT_ALLOWED_HERE = 1344 Error "A label is not allowed here.";
    DUPLICATE_IDENTIFIER = 2300 Error "Duplicate identifier '{0}'.";
    CONFLICTS_WITH_GLOBAL_THIS = 2397 Error "Declaration name conflicts with built-in global identifier '{0}'.";
    AMBIENT_MODULE_NESTED = 2435 Error "Ambient modules cannot be nested in other modules or namespaces.";
    CANNOT_REDECLARE_BLOCK_SCOPED = 2451 Error "Cannot redeclare block-scoped variable '{0}'.";
    MULTIPLE_DEFAULT_EXPORTS = 2528 Error "A module cannot have multiple default exports.";
    ENUM_MERGE_CONFLICT = 2567 Error "Enum declarations can only merge with namespace or other enum declarations.";
    INVALID_AUGMENTATION_MODULE = 2664 Error "Invalid module name in augmentation, module '{0}' cannot be found.";
    AUGMENT_NON_MODULE_ENTITY = 2671 Error "Cannot augment module '{0}' because it resolves to a non-module entity.";
    GLOBAL_AUGMENTATION_PLACEMENT = 2669 Error "Augmentations for the global scope can only be directly nested in external modules or ambient module declarations.";
    ALSO_DECLARED_HERE = 6203 Message "'{0}' was also declared here.";
    UNREACHABLE_CODE = 7027 Error "Unreachable code detected.";
    UNUSED_LABEL = 7028 Error "Unused label.";
    MAXIMUM_NESTING_DEPTH = 99001 Error "Maximum nesting depth exceeded; the rest of this construct was skipped.";
}
