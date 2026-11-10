; PowerShell highlights for Nimbus. Started from the query in
; tree-sitter-powershell 0.26.4 (MIT licence, see
; https://github.com/airbus-cert/tree-sitter-powershell), queries/
; highlights.scm, rewritten to use the capture names Nimbus themes colour
; and to cover parameters, attributes, hash keys, classes and the
; automatic variables.
;
; The innermost node wins, and for the same node the later pattern wins,
; so general rules come first and their exceptions after.

; Keywords. The grammar matches them in any case (`If`, `FOREACH`).

[
  "param" "dynamicparam" "begin" "process" "end"
  "if" "elseif" "else" "switch" "foreach" "for" "in" "while" "do" "until"
  "function" "filter" "workflow"
  "break" "continue" "throw" "return" "exit" "trap"
  "try" "catch" "finally"
  "data" "inlinescript" "parallel" "sequence"
] @keyword

; Operators. The dash operators (`-eq`, `-match`) read as words, so they
; share the keyword colour, as most PowerShell themes do.

(comparison_operator) @keyword.operator
(format_operator) @keyword.operator
[
  "-and" "-or" "-xor" "-not" "-band" "-bor" "-bxor" "-bnot" "-join"
  "-split" "-shl" "-shr" "-as" "-is" "-isnot" "-f"
] @keyword.operator

[
  "=" "+=" "-=" "*=" "/=" "%="
  "+" "-" "*" "/" "%" "\\" ".." "!" "++" "--"
  "|" "||" "&&" "&" "::"
  ">" ">>" "2>" "2>>" "2>&1" "*>" "*>>"
] @operator

(command_invokation_operator) @operator
(file_redirection_operator) @operator
(merging_redirection_operator) @operator

; Punctuation.

["(" ")" "[" "]" "{" "}" "@(" "@{" "$(" ] @punctuation.bracket
[";" "," "."] @punctuation.delimiter

; Literals.

(string_literal) @string
(integer_literal) @number
(real_literal) @number

; Variables: plain in code and inside double-quoted strings, so an
; interpolated `$name` stands out from the text around it.

(variable) @variable

((variable) @constant.builtin
 (#match? @constant.builtin "^\\$(true|false|null)$"))

((variable) @variable.builtin
 (#match? @variable.builtin "^\\$(_|this|PSItem|args|input|PSScriptRoot|PSCommandPath|PSBoundParameters|MyInvocation|LASTEXITCODE|Error|Host|HOME|PWD)$"))

; `$_`, `$$`, `$?` and `$^` are tokens of their own.
(variable ["$_" "$$" "$?" "$^"] @variable.builtin)

; `$env:PATH`, `$script:count`: the scope or drive before the colon.
((variable) @variable.builtin
 (#match? @variable.builtin "^\\$(env|global|script|local|private|using):"))

; Commands and functions.

(command
  command_name: (command_name) @function)
(function_statement
  (function_name) @function)
(command_parameter) @label

; Members: `.Length` is a property, `.ToString()` and `::Parse()` a call.

(member_access
  (member_name) @property)
(invokation_expression
  (member_name) @function.method)

; Types, attributes, classes and enums.

(type_spec) @type
(attribute
  (attribute_name) @attribute)
(attribute_argument
  (simple_name) @property)
(class_statement
  (simple_name) @type)
(class_method_definition
  (simple_name) @function.method)
(enum_statement
  (simple_name) @type)
(enum_member
  (simple_name) @constant)

; Hash table keys: `@{ Name = 1 }`.

(key_expression) @property

(label) @label
(comment) @comment
