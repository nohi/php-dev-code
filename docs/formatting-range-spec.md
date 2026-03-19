# Range Formatting Specification

## Scope

This document defines the behavior of range formatting in the Rust language server.

- Entry point: `textDocument/rangeFormatting`
- Implementation: `format_range_line_edit` in `server/src/formatting.rs`

## Behavioral Rules

1. Requested range is line/column clamped to existing document bounds.
2. Only the selected slice is normalized by range-format rules.
3. Text outside the selected columns is preserved exactly.
4. If formatting results in no content change, no edit is returned.

## Normalization Rules (inside selected range)

1. Trim trailing whitespace from each selected line.
2. If content looks like Blade, normalize directive spacing:
   - `@if(` -> `@if (`
   - same rule for `@elseif`, `@foreach`, `@for`, `@while`, `@section`, `@extends`, `@include`
3. Remove trailing empty lines at the end of the selected slice.
4. Collapse 3 or more consecutive empty lines to 2.

## Partial-Line Semantics

For selections starting/ending in the middle of a line:

- Prefix before `start.character` is preserved.
- Suffix after `end.character` is preserved.
- Only the middle selected segment is reformatted and reinserted.

This is intentionally different from full-document formatting, which can replace the whole document text.

## On-Type Current-Line Formatting

`textDocument/onTypeFormatting` reuses range formatting by requesting:

- start: `(line, 0)`
- end: `(line, u32::MAX)`

Because of clamping, this means the full current line is formatted.

## Non-Goals

- No global reindent pass.
- No PSR-wide structural rewrite in range mode.
- No edits outside the requested start/end line window, except preserved prefix/suffix on boundary lines.

## Example

Given line selection from inside line 2 to inside line 5:

- Source boundary text outside the selection remains byte-for-byte intact.
- Selected content has trailing spaces removed and excessive blank lines compacted.
- If resulting selected content is identical, server returns an empty edit list.
