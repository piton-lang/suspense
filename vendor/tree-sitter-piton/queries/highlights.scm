; Piton highlighting.
;
; Structural questions — what a name resolves to, which base supplied a value —
; come from the language server's semantic tokens. These queries colour what can
; be known from the text alone.

(comment) @comment

; Declarations
"export" @keyword
"abstract" @keyword.modifier
"as" @keyword
"extends" @keyword
(pass_statement) @keyword

(anchor_declaration
  keyword: (declaration_keyword) @keyword.type
  name: (identifier) @type.definition)

; A user keyword is sugar for `extends`, so it reads as the thing it declares.
(anchor_declaration alias: (keyword_name) @function.macro)
(base_list (identifier) @type)

; Imports
"use" @keyword.import
"from" @keyword.import
"import" @keyword.import
(from_declaration direction: _ @keyword.import)
(module_path) @string.special.path
(import_item name: (identifier) @variable)
(import_item alias: (identifier) @variable.parameter)

; Properties
(property name: (key) @property)
(type_constraint ":" @punctuation.delimiter)
(type_constraint "extends" @keyword)
(builtin_type) @type.builtin
(type_constraint type: (identifier) @type)
(list_suffix) @punctuation.bracket

; Structure markers
(list_item "-" @punctuation.special)
(merge_item operator: _ @operator)

; Interpolation
(interpolation (sigil) @punctuation.special)
(interpolation "}" @punctuation.special)
(constant) @constant.builtin
(self_reference) @variable.builtin
(number) @number
(string) @string
(operator) @operator
(identifier) @variable

; An anchor name is capitalised by convention, and reads better as a type.
((identifier) @type
  (#match? @type "^[A-Z]"))

; Verbatim regions
(fence) @string
(fence_marker) @punctuation.special
(fence_language) @attribute
(escape_block) @string
(escape_marker) @punctuation.special

; Prose is a value in Piton, not decoration.
(text) @string
