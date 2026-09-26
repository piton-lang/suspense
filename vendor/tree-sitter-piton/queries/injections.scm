; A fenced block's language tag selects a parser for its contents, so a `json`
; example inside a specification is highlighted as JSON.

((fence
   language: (fence_language) @injection.language
   (fence_content) @injection.content)
 (#set! injection.include-children))
