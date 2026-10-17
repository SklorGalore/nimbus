; Symbols for Markdown: its headings (Go to Symbol, M14). The grammar
; ships no tags query, so this one is ours.
(atx_heading
  heading_content: (inline) @name) @definition.heading

(setext_heading
  heading_content: (paragraph) @name) @definition.heading
