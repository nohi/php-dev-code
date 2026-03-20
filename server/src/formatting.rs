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

/// Generates PHPDoc stub when user presses Enter after typing `/**`.
///
/// `trigger_line` is the 0-based line index of the `/**` line.
/// Returns TextEdit(s) that insert the PHPDoc body content.
pub(crate) fn generate_phpdoc_on_enter(text: &str, trigger_line: u32) -> Option<Vec<TextEdit>> {
    let lines: Vec<&str> = text.lines().collect();
    let trigger_idx = trigger_line as usize;

    let trigger_content = lines.get(trigger_idx)?.trim();
    if !trigger_content.starts_with("/**") {
        return None;
    }
    // Only trigger for the bare opener (not a complete block like /** ... */)
    if trigger_content.ends_with("*/") && trigger_content.len() > 3 {
        return None;
    }

    let raw_trigger_line = lines[trigger_idx];
    let indent_len = raw_trigger_line.len() - raw_trigger_line.trim_start().len();
    let indent = &raw_trigger_line[..indent_len];

    // Look ahead for a function/class/interface/trait declaration (skip blank lines)
    let mut next_decl_line: Option<usize> = None;
    for i in (trigger_idx + 1)..lines.len().min(trigger_idx + 10) {
        let l = lines[i].trim();
        if !l.is_empty() {
            next_decl_line = Some(i);
            break;
        }
    }

    let mut body_lines: Vec<String> = vec![format!("{} *", indent)];

    if let Some(decl_idx) = next_decl_line {
        let sig = collect_signature_lines(&lines, decl_idx);

        let params = parse_function_params(&sig);
        for (type_hint, name) in &params {
            if type_hint.is_empty() {
                body_lines.push(format!("{} * @param mixed {}", indent, name));
            } else {
                body_lines.push(format!("{} * @param {} {}", indent, type_hint, name));
            }
        }

        if let Some(ret) = parse_return_type(&sig) {
            body_lines.push(format!("{} * @return {}", indent, ret));
        } else if sig.contains("function ") {
            body_lines.push(format!("{} * @return void", indent));
        }
    }

    body_lines.push(format!("{} */", indent));

    let insert_text = body_lines.join("\n") + "\n";

    let insert_line = trigger_line + 1;
    let replace_end_col = lines
        .get(insert_line as usize)
        .map(|l| l.len() as u32)
        .unwrap_or(0);

    let range = Range::new(
        Position::new(insert_line, 0),
        Position::new(insert_line, replace_end_col),
    );

    Some(vec![TextEdit::new(range, insert_text)])
}

/// Collect a function/class declaration signature across potentially multiple lines.
fn collect_signature_lines(lines: &[&str], start: usize) -> String {
    let mut sig = String::new();
    for i in start..lines.len().min(start + 8) {
        sig.push_str(lines[i].trim());
        sig.push(' ');
        if lines[i].contains('{') || lines[i].ends_with(';') {
            break;
        }
    }
    sig
}

/// Parse function parameters from a function signature string.
/// Returns Vec of (type_hint, $name).
fn parse_function_params(sig: &str) -> Vec<(String, String)> {
    let open = sig.find('(');
    let close = sig.rfind(')');
    let (open, close) = match (open, close) {
        (Some(o), Some(c)) if c > o => (o, c),
        _ => return Vec::new(),
    };

    let params_str = &sig[open + 1..close];
    if params_str.trim().is_empty() {
        return Vec::new();
    }

    let mut result = Vec::new();
    let mut depth = 0i32;
    let mut current = String::new();

    for ch in params_str.chars() {
        match ch {
            '(' | '[' | '<' => { depth += 1; current.push(ch); }
            ')' | ']' | '>' => { depth -= 1; current.push(ch); }
            ',' if depth == 0 => {
                if let Some(param) = parse_single_param(current.trim()) {
                    result.push(param);
                }
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        if let Some(param) = parse_single_param(current.trim()) {
            result.push(param);
        }
    }

    result
}

/// Parse a single parameter like `?SomeClass $foo = null` → ("?SomeClass", "$foo")
fn parse_single_param(param: &str) -> Option<(String, String)> {
    let param = param.trim_start_matches(|c| c == '&' || c == '.');
    let param = param.trim();
    if param.is_empty() {
        return None;
    }

    let tokens: Vec<&str> = param.split_whitespace().collect();
    let var_pos = tokens.iter().position(|t| t.starts_with('$'))?;
    let var_name = tokens[var_pos].trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '$');

    let type_hint = if var_pos > 0 {
        tokens[..var_pos].join(" ")
    } else {
        String::new()
    };

    Some((type_hint, var_name.to_string()))
}

/// Extract the return type from a function signature like `... ): ReturnType {`
fn parse_return_type(sig: &str) -> Option<String> {
    let close_paren = sig.rfind(')')?;
    let after = sig.get(close_paren + 1..)?;

    let colon_pos = after.find(':')?;
    let type_part = after.get(colon_pos + 1..)?.trim();

    let end = type_part.find(|c: char| c == '{' || c == ';')
        .unwrap_or(type_part.len());
    let ret = type_part[..end].trim();

    if ret.is_empty() || ret == "void" {
        None
    } else {
        Some(ret.to_string())
    }
}
