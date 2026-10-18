; Symbols for Rust (Go to Symbol, M14): tree-sitter-rust's tags.scm with
; two changes, marked below: methods are only functions in an impl or
; trait body, and constants and statics are listed.

; ADT definitions

(struct_item
    name: (type_identifier) @name) @definition.class

(enum_item
    name: (type_identifier) @name) @definition.class

(union_item
    name: (type_identifier) @name) @definition.class

; type aliases

(type_item
    name: (type_identifier) @name) @definition.class

; method definitions: functions in an impl or trait body. Upstream marks
; every function inside a declaration_list, which makes the functions of
; a `mod` methods too.

(impl_item
    body: (declaration_list
        (function_item
            name: (identifier) @name) @definition.method))

(trait_item
    body: (declaration_list
        (function_item
            name: (identifier) @name) @definition.method))

; function definitions

(function_item
    name: (identifier) @name) @definition.function

; trait definitions
(trait_item
    name: (type_identifier) @name) @definition.interface

; module definitions
(mod_item
    name: (identifier) @name) @definition.module

; macro definitions

(macro_definition
    name: (identifier) @name) @definition.macro

; references

(call_expression
    function: (identifier) @name) @reference.call

(call_expression
    function: (field_expression
        field: (field_identifier) @name)) @reference.call

(macro_invocation
    macro: (identifier) @name) @reference.call

; implementations

(impl_item
    trait: (type_identifier) @name) @reference.implementation

(impl_item
    type: (type_identifier) @name
    !trait) @reference.implementation

; constants and statics (added)

(const_item
    name: (identifier) @name) @definition.constant

(static_item
    name: (identifier) @name) @definition.constant
