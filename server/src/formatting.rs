use tower_lsp::lsp_types::{Position, Range, TextEdit};

pub(crate) fn format_document(text: &str) -> String {
    let mut normalized_lines = text
        .split('\n')
        .map(|line| line.trim_end().to_string())
        .collect::<Vec<_>>();

    if looks_like_blade_template(text) {
        normalized_lines = normalized_lines
            .into_iter()
            .map(|line| format_blade_directive_spacing(&line))
            .collect();
    }

    while normalized_lines
        .last()
        .map(|line| line.is_empty())
        .unwrap_or(false)
    {
        normalized_lines.pop();
    }

    let mut compact = Vec::new();
    let mut consecutive_empty = 0usize;
    for line in normalized_lines {
        if line.is_empty() {
            consecutive_empty += 1;
            if consecutive_empty <= 2 {
                compact.push(line);
            }
        } else {
            consecutive_empty = 0;
            compact.push(line);
        }
    }

    if compact.is_empty() {
        return String::new();
    }

    let mut out = compact.join("\n");
    out.push('\n');
    out
}

pub(crate) fn document_end_position(text: &str) -> Position {
    if text.is_empty() {
        return Position::new(0, 0);
    }

    let lines = text.split('\n').collect::<Vec<_>>();
    let end_line = lines.len().saturating_sub(1) as u32;
    let end_char = lines
        .last()
        .map(|line| line.chars().count() as u32)
        .unwrap_or(0);
    Position::new(end_line, end_char)
}

pub(crate) fn format_range_line_edit(text: &str, requested: Range) -> Option<TextEdit> {
    if text.is_empty() {
        return None;
    }

    let lines = text.split('\n').collect::<Vec<_>>();
    let line_count = lines.len();
    if line_count == 0 {
        return None;
    }

    let start_line = (requested.start.line as usize).min(line_count.saturating_sub(1));
    let end_line = (requested.end.line as usize).min(line_count.saturating_sub(1));
    if start_line > end_line {
        return None;
    }

    let start_line_chars = lines[start_line].chars().count();
    let end_line_chars = lines[end_line].chars().count();
    let start_col = (requested.start.character as usize).min(start_line_chars);
    let end_col = (requested.end.character as usize).min(end_line_chars);

    let original_range_text = if start_line == end_line {
        char_slice(lines[start_line], start_col, end_col)
    } else {
        let mut parts = Vec::new();
        parts.push(char_slice(lines[start_line], start_col, start_line_chars));
        for middle in (start_line + 1)..end_line {
            parts.push(lines[middle].to_string());
        }
        parts.push(char_slice(lines[end_line], 0, end_col));
        parts.join("\n")
    };

    let formatted = format_range_text(&original_range_text);
    if formatted == original_range_text {
        return None;
    }

    let new_text = if start_line == end_line {
        let prefix = char_slice(lines[start_line], 0, start_col);
        let suffix = char_slice(lines[start_line], end_col, start_line_chars);
        format!("{}{}{}", prefix, formatted, suffix)
    } else {
        let prefix = char_slice(lines[start_line], 0, start_col);
        let suffix = char_slice(lines[end_line], end_col, end_line_chars);
        format!("{}{}{}", prefix, formatted, suffix)
    };

    Some(TextEdit {
        range: Range::new(
            Position::new(start_line as u32, start_col as u32),
            Position::new(end_line as u32, end_col as u32),
        ),
        new_text,
    })
}

pub(crate) fn format_current_line_edit(text: &str, line: u32) -> Option<TextEdit> {
    let requested = Range::new(Position::new(line, 0), Position::new(line, u32::MAX));
    format_range_line_edit(text, requested)
}

pub(crate) fn format_range_text(text: &str) -> String {
    let mut normalized_lines = text
        .split('\n')
        .map(|line| line.trim_end().to_string())
        .collect::<Vec<_>>();

    if looks_like_blade_template(text) {
        normalized_lines = normalized_lines
            .into_iter()
            .map(|line| format_blade_directive_spacing(&line))
            .collect();
    }

    while normalized_lines
        .last()
        .map(|line| line.is_empty())
        .unwrap_or(false)
    {
        normalized_lines.pop();
    }

    let mut compact = Vec::new();
    let mut consecutive_empty = 0usize;
    for line in normalized_lines {
        if line.is_empty() {
            consecutive_empty += 1;
            if consecutive_empty <= 2 {
                compact.push(line);
            }
        } else {
            consecutive_empty = 0;
            compact.push(line);
        }
    }

    compact.join("\n")
}

pub(crate) fn format_blade_directive_spacing(line: &str) -> String {
    let mut out = line.to_string();
    for directive in ["@if", "@elseif", "@foreach", "@for", "@while", "@section", "@extends", "@include"] {
        let tight = format!("{}(", directive);
        let spaced = format!("{} (", directive);
        out = out.replace(&tight, &spaced);
    }
    out
}

pub(crate) fn looks_like_blade_template(text: &str) -> bool {
    text.contains("@extends(")
        || text.contains("@section(")
        || text.contains("@yield(")
        || text.contains("@if(")
        || text.contains("@foreach(")
        || text.contains("{{")
        || text.contains("{!!")
}

    fn char_slice(input: &str, start: usize, end: usize) -> String {
        input
        .chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
    }
