; Definitions for Go to Symbol in PowerShell: functions and filters,
; classes with their methods, and enums. Nimbus's own; the grammar ships
; no tags query.

(function_statement
  (function_name) @name) @definition.function

(class_statement
  (simple_name) @name) @definition.class

(class_method_definition
  (simple_name) @name) @definition.method

(enum_statement
  (simple_name) @name) @definition.class
