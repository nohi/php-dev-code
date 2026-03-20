use std::collections::{HashMap, HashSet};
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range};

use crate::{
    code_mask_for_line,
    comment_text_for_line,
    extract_symbols,
    find_identifier_ranges,
    parse_function_parameters,
    parse_single_use_entry,
    SymbolKind,
    token_after_keyword,
};
pub(crate) fn detect_undefined_variables(text: &str) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let mut global_declared: HashSet<String> = HashSet::new();
    let mut function_scopes: Vec<(i32, HashSet<String>)> = Vec::new();
    let mut pending_function_params: Option<HashSet<String>> = None;
    let mut brace_depth: i32 = 0;
    let mut in_block_comment = false;
    let mut in_annotation_block_comment = false;

    for (line_idx, line) in text.lines().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let mask = code_mask_for_line(&chars, &mut in_block_comment);
        let line_open = chars
            .iter()
            .enumerate()
            .filter(|(idx, ch)| **ch == '{' && mask.get(*idx).copied().unwrap_or(false))
            .count() as i32;
        let line_close = chars
            .iter()
            .enumerate()
            .filter(|(idx, ch)| **ch == '}' && mask.get(*idx).copied().unwrap_or(false))
            .count() as i32;

        let sanitized_line = chars
            .iter()
            .enumerate()
            .map(|(idx, ch)| if mask.get(idx).copied().unwrap_or(false) { *ch } else { ' ' })
            .collect::<String>();
        let comment_only_line = comment_text_for_line(&chars, &mut in_annotation_block_comment);

        let mut signature_declared_for_line: HashSet<String> = HashSet::new();

        for annotated_var in extract_var_annotation_variables(&comment_only_line) {
            if let Some((_, scope)) = function_scopes.last_mut() {
                scope.insert(annotated_var);
            } else {
                global_declared.insert(annotated_var);
            }
        }

        // Function parameters are considered declared in the current scope.
        if sanitized_line.contains("function") {
            let mut params = HashSet::new();
            for param in parse_function_parameters(&sanitized_line) {
                if let Some(var) = extract_first_variable_name(&param) {
                    signature_declared_for_line.insert(var.clone());
                    params.insert(var);
                }
            }

            if sanitized_line.contains('{') {
                function_scopes.push((brace_depth + 1, params));
            } else {
                pending_function_params = Some(params);
            }

            for closure_var in extract_closure_use_variables(&sanitized_line) {
                if let Some((_, scope)) = function_scopes.last_mut() {
                    scope.insert(closure_var.clone());
                }
                signature_declared_for_line.insert(closure_var);
            }
        } else if pending_function_params.is_some() && line_open > 0 {
            let params = pending_function_params.take().unwrap_or_default();
            function_scopes.push((brace_depth + 1, params));
        }

        for name in extract_declared_variables_after_keyword(&sanitized_line, "global") {
            if let Some((_, scope)) = function_scopes.last_mut() {
                scope.insert(name.clone());
            }
            global_declared.insert(name);
        }

        for name in extract_declared_variables_after_keyword(&sanitized_line, "static") {
            if let Some((_, scope)) = function_scopes.last_mut() {
                scope.insert(name);
            } else {
                global_declared.insert(name);
            }
        }

        if sanitized_line.contains("catch") {
            if let Some(name) = extract_first_variable_name(&sanitized_line) {
                if let Some((_, scope)) = function_scopes.last_mut() {
                    scope.insert(name.clone());
                }
                signature_declared_for_line.insert(name);
            }
        }

        for foreach_var in extract_foreach_declared_variables(&sanitized_line) {
            if let Some((_, scope)) = function_scopes.last_mut() {
                scope.insert(foreach_var);
            } else {
                global_declared.insert(foreach_var);
            }
        }

        let occurrences = variable_occurrences_in_line(&chars, &mask);
        for (name, start, end_exclusive) in occurrences {
            if is_builtin_variable(&name) {
                continue;
            }

            let is_declared = signature_declared_for_line.contains(&name)
                || function_scopes
                    .last()
                    .map(|(_, scope)| scope.contains(&name))
                    .unwrap_or(false)
                || global_declared.contains(&name);

            if is_assignment_declaration(&chars, end_exclusive, &mask) {
                if let Some((_, scope)) = function_scopes.last_mut() {
                    scope.insert(name);
                } else {
                    global_declared.insert(name);
                }
                continue;
            }

            if !is_declared {
                diagnostics.push(Diagnostic {
                    range: Range::new(
                        Position::new(line_idx as u32, start as u32),
                        Position::new(line_idx as u32, end_exclusive as u32),
                    ),
                    severity: Some(DiagnosticSeverity::WARNING),
                    message: format!("Undefined variable: {name}"),
                    source: Some("vscode-ls-php".to_string()),
                    ..Diagnostic::default()
                });
            }
        }

        brace_depth = (brace_depth + line_open - line_close).max(0);
        while let Some((scope_depth, _)) = function_scopes.last() {
            if brace_depth < *scope_depth {
                function_scopes.pop();
            } else {
                break;
            }
        }
    }

    diagnostics
}

pub(crate) fn detect_unused_variables(text: &str) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let mut function_scopes: Vec<(i32, HashMap<String, (u32, u32, u32, bool)>)> = Vec::new();
    let mut pending_function = false;
    let mut brace_depth: i32 = 0;
    let mut in_block_comment = false;

    for (line_idx, line) in text.lines().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let mask = code_mask_for_line(&chars, &mut in_block_comment);
        let line_open = chars
            .iter()
            .enumerate()
            .filter(|(idx, ch)| **ch == '{' && mask.get(*idx).copied().unwrap_or(false))
            .count() as i32;
        let line_close = chars
            .iter()
            .enumerate()
            .filter(|(idx, ch)| **ch == '}' && mask.get(*idx).copied().unwrap_or(false))
            .count() as i32;

        let sanitized_line = chars
            .iter()
            .enumerate()
            .map(|(idx, ch)| if mask.get(idx).copied().unwrap_or(false) { *ch } else { ' ' })
            .collect::<String>();

        if sanitized_line.contains("function") {
            if sanitized_line.contains('{') {
                function_scopes.push((brace_depth + 1, HashMap::new()));
            } else {
                pending_function = true;
            }
        } else if pending_function && line_open > 0 {
            function_scopes.push((brace_depth + 1, HashMap::new()));
            pending_function = false;
        }

        let occurrences = variable_occurrences_in_line(&chars, &mask);
        for (name, start, end_exclusive) in occurrences {
            if is_builtin_variable(&name) {
                continue;
            }

            let Some((_, scope)) = function_scopes.last_mut() else {
                continue;
            };

            let prev_non_space = chars[..start]
                .iter()
                .enumerate()
                .rev()
                .find(|(idx, ch)| mask.get(*idx).copied().unwrap_or(false) && !ch.is_whitespace())
                .map(|(_, ch)| *ch);

            // Keep this heuristic conservative: treat reference and variable-variable
            // forms as usage to avoid noisy false positives.
            if matches!(prev_non_space, Some('&' | '$')) {
                if let Some(entry) = scope.get_mut(&name) {
                    entry.3 = true;
                }
                continue;
            }

            if is_assignment_declaration(&chars, end_exclusive, &mask) {
                scope
                    .entry(name)
                    .or_insert((line_idx as u32, start as u32, end_exclusive as u32, false));
                continue;
            }

            if let Some(entry) = scope.get_mut(&name) {
                entry.3 = true;
            }
        }

        brace_depth = (brace_depth + line_open - line_close).max(0);
        while let Some((scope_depth, _)) = function_scopes.last() {
            if brace_depth < *scope_depth {
                let (_, scope) = function_scopes.pop().unwrap_or_default();
                for (name, (line, start, end, used)) in scope {
                    if used {
                        continue;
                    }

                    diagnostics.push(Diagnostic {
                        range: Range::new(Position::new(line, start), Position::new(line, end)),
                        severity: Some(DiagnosticSeverity::HINT),
                        message: format!("Unused variable: {name}"),
                        source: Some("vscode-ls-php".to_string()),
                        ..Diagnostic::default()
                    });
                }
            } else {
                break;
            }
        }
    }

    diagnostics
}

pub(crate) fn detect_brace_mismatch(text: &str) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let mut in_block_comment = false;
    let mut stack: Vec<(u32, u32)> = Vec::new();

    for (line_idx, line) in text.lines().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let mask = code_mask_for_line(&chars, &mut in_block_comment);

        for (idx, ch) in chars.iter().enumerate() {
            if !mask.get(idx).copied().unwrap_or(false) {
                continue;
            }

            if *ch == '{' {
                stack.push((line_idx as u32, idx as u32));
                continue;
            }

            if *ch == '}' {
                if stack.pop().is_none() {
                    diagnostics.push(Diagnostic {
                        range: Range::new(
                            Position::new(line_idx as u32, idx as u32),
                            Position::new(line_idx as u32, idx as u32 + 1),
                        ),
                        severity: Some(DiagnosticSeverity::ERROR),
                        message: "Unexpected closing brace '}'".to_string(),
                        source: Some("vscode-ls-php".to_string()),
                        ..Diagnostic::default()
                    });
                }
            }
        }
    }

    for (line, character) in stack {
        diagnostics.push(Diagnostic {
            range: Range::new(
                Position::new(line, character),
                Position::new(line, character + 1),
            ),
            severity: Some(DiagnosticSeverity::ERROR),
            message: "Unclosed opening brace '{'".to_string(),
            source: Some("vscode-ls-php".to_string()),
            ..Diagnostic::default()
        });
    }

    diagnostics
}

pub(crate) fn detect_operator_confusion(text: &str) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let mut in_block_comment = false;

    for (line_idx, line) in text.lines().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let mask = code_mask_for_line(&chars, &mut in_block_comment);
        let sanitized = chars
            .iter()
            .enumerate()
            .map(|(idx, ch)| if mask.get(idx).copied().unwrap_or(false) { *ch } else { ' ' })
            .collect::<String>();

        for keyword in ["if", "while", "elseif"] {
            for (condition, start_col) in condition_segments_for_keyword(&sanitized, keyword) {
                if let Some(offset) = suspicious_assignment_offset(&condition) {
                    let column = start_col + offset as u32;
                    diagnostics.push(Diagnostic {
                        range: Range::new(
                            Position::new(line_idx as u32, column),
                            Position::new(line_idx as u32, column + 1),
                        ),
                        severity: Some(DiagnosticSeverity::WARNING),
                        message: "Suspicious assignment '=' in conditional expression".to_string(),
                        source: Some("vscode-ls-php".to_string()),
                        ..Diagnostic::default()
                    });
                }
            }
        }
    }

    diagnostics
}

pub(crate) fn detect_comment_task_markers(text: &str) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let mut in_block_comment = false;

    for (line_idx, line) in text.lines().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let comment_line = comment_text_for_line(&chars, &mut in_block_comment);

        for (marker, label) in [("todo", "TODO"), ("fixme", "FIXME")] {
            for start in comment_marker_offsets(&comment_line, marker) {
                diagnostics.push(Diagnostic {
                    range: Range::new(
                        Position::new(line_idx as u32, start as u32),
                        Position::new(line_idx as u32, (start + marker.len()) as u32),
                    ),
                    severity: Some(DiagnosticSeverity::HINT),
                    message: format!("Comment task marker: {label}"),
                    source: Some("vscode-ls-php".to_string()),
                    ..Diagnostic::default()
                });
            }
        }
    }

    diagnostics
}

fn comment_marker_offsets(comment_line: &str, marker: &str) -> Vec<usize> {
    let lowered = comment_line
        .chars()
        .map(|ch| ch.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let marker_chars = marker.chars().collect::<Vec<_>>();
    if marker_chars.is_empty() || lowered.len() < marker_chars.len() {
        return Vec::new();
    }

    let marker_len = marker_chars.len();
    let mut out = Vec::new();

    for i in 0..=(lowered.len() - marker_len) {
        if lowered[i..(i + marker_len)] != marker_chars[..] {
            continue;
        }

        let prev = if i == 0 { ' ' } else { lowered[i - 1] };
        let next = if i + marker_len >= lowered.len() {
            ' '
        } else {
            lowered[i + marker_len]
        };

        if marker_boundary(prev) && marker_boundary(next) {
            out.push(i);
        }
    }

    out
}

fn marker_boundary(ch: char) -> bool {
    !ch.is_ascii_alphanumeric() && ch != '_'
}

pub(crate) fn detect_php_version_compatibility(
    text: &str,
    target_version: (u32, u32),
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let mut in_block_comment = false;

    for (line_idx, line) in text.lines().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let mask = code_mask_for_line(&chars, &mut in_block_comment);
        let sanitized = chars
            .iter()
            .enumerate()
            .map(|(idx, ch)| if mask.get(idx).copied().unwrap_or(false) { *ch } else { ' ' })
            .collect::<String>();
        let trimmed = sanitized.trim_start();
        let trim_offset = sanitized.len().saturating_sub(trimmed.len());

        if version_less_than(target_version, (8, 0)) {
            if let Some(col) = nullsafe_operator_offset(&chars, &mask) {
                diagnostics.push(version_compat_diagnostic(
                    line_idx as u32,
                    col as u32,
                    (col + 3) as u32,
                    target_version,
                    (8, 0),
                    "nullsafe operator ?->",
                ));
            }

            if let Some(col) = keyword_call_offset(trimmed, "match") {
                let actual_col = col + trim_offset;
                diagnostics.push(version_compat_diagnostic(
                    line_idx as u32,
                    actual_col as u32,
                    (actual_col + 5) as u32,
                    target_version,
                    (8, 0),
                    "match expression",
                ));
            }
        }

        if version_less_than(target_version, (8, 1)) {
            if let Some(col) = token_after_keyword_offset(trimmed, "enum") {
                let actual_col = col + trim_offset;
                diagnostics.push(version_compat_diagnostic(
                    line_idx as u32,
                    actual_col as u32,
                    (actual_col + 4) as u32,
                    target_version,
                    (8, 1),
                    "enum declaration",
                ));
            }

            if let Some(col) = token_after_keyword_offset(trimmed, "readonly") {
                let actual_col = col + trim_offset;
                diagnostics.push(version_compat_diagnostic(
                    line_idx as u32,
                    actual_col as u32,
                    (actual_col + 8) as u32,
                    target_version,
                    (8, 1),
                    "readonly keyword",
                ));
            }
        }
    }

    diagnostics
}

fn version_compat_diagnostic(
    line: u32,
    start: u32,
    end: u32,
    target: (u32, u32),
    required: (u32, u32),
    feature: &str,
) -> Diagnostic {
    Diagnostic {
        range: Range::new(Position::new(line, start), Position::new(line, end)),
        severity: Some(DiagnosticSeverity::WARNING),
        message: format!(
            "PHP version compatibility: '{}' requires PHP {}.{}+, target is {}.{}",
            feature, required.0, required.1, target.0, target.1
        ),
        source: Some("vscode-ls-php".to_string()),
        ..Diagnostic::default()
    }
}

fn version_less_than(target: (u32, u32), required: (u32, u32)) -> bool {
    target.0 < required.0 || (target.0 == required.0 && target.1 < required.1)
}

fn nullsafe_operator_offset(chars: &[char], mask: &[bool]) -> Option<usize> {
    if chars.len() < 3 {
        return None;
    }
    for i in 0..=(chars.len() - 3) {
        if mask.get(i).copied().unwrap_or(false)
            && mask.get(i + 1).copied().unwrap_or(false)
            && mask.get(i + 2).copied().unwrap_or(false)
            && chars[i] == '?'
            && chars[i + 1] == '-'
            && chars[i + 2] == '>'
        {
            return Some(i);
        }
    }
    None
}

fn keyword_call_offset(trimmed_line: &str, keyword: &str) -> Option<usize> {
    let chars = trimmed_line.chars().collect::<Vec<_>>();
    let keyword_chars = keyword.chars().collect::<Vec<_>>();
    if chars.len() < keyword_chars.len() {
        return None;
    }

    let mut i = 0usize;
    while i + keyword_chars.len() <= chars.len() {
        if chars[i..(i + keyword_chars.len())] != keyword_chars[..] {
            i += 1;
            continue;
        }

        let before_ok = i == 0 || !chars[i - 1].is_ascii_alphanumeric();
        let mut j = i + keyword_chars.len();
        while j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        let after_ok = j < chars.len() && chars[j] == '(';
        if before_ok && after_ok {
            return Some(i);
        }

        i += 1;
    }

    None
}

fn token_after_keyword_offset(trimmed_line: &str, keyword: &str) -> Option<usize> {
    if !trimmed_line.starts_with(keyword) {
        return None;
    }

    let tail = &trimmed_line[keyword.len()..];
    if tail.is_empty() {
        return Some(0);
    }

    let first = tail.chars().next();
    if matches!(first, Some(ch) if ch.is_whitespace()) {
        return Some(0);
    }

    None
}

pub(crate) fn detect_undefined_function_calls(text: &str) -> Vec<Diagnostic> {
    let known = HashSet::new();
    detect_undefined_function_calls_with_known(text, &known)
}

pub(crate) fn detect_undefined_function_calls_with_known(
    text: &str,
    known_functions: &HashSet<String>,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let mut defined_functions: HashSet<String> = extract_symbols(text)
        .into_iter()
        .filter(|s| s.kind == SymbolKind::FUNCTION)
        .flat_map(|s| {
            let short = s.name.to_ascii_lowercase();
            let fqn = s.fqn().to_ascii_lowercase();
            [short, fqn]
        })
        .collect();
    defined_functions.extend(known_functions.iter().cloned());

    let mut in_block_comment = false;
    for (line_idx, line) in text.lines().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let mask = code_mask_for_line(&chars, &mut in_block_comment);

        let mut i = 0usize;
        while i < chars.len() {
            let is_ident_start = chars[i].is_ascii_alphabetic()
                || chars[i] == '_'
                || (chars[i] == '\\'
                    && i + 1 < chars.len()
                    && (chars[i + 1].is_ascii_alphabetic() || chars[i + 1] == '_'));
            if !mask.get(i).copied().unwrap_or(false) || !is_ident_start {
                i += 1;
                continue;
            }

            let start = i;
            i += 1;
            while i < chars.len()
                && mask.get(i).copied().unwrap_or(false)
                && (chars[i].is_ascii_alphanumeric() || chars[i] == '_' || chars[i] == '\\')
            {
                i += 1;
            }

            let name: String = chars[start..i].iter().collect();
            let canonical_name = name.trim_start_matches('\\');
            if !canonical_name.contains('\\') {
                continue;
            }
            let lower_name = canonical_name.to_ascii_lowercase();

            let mut j = i;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j >= chars.len() || chars[j] != '(' {
                continue;
            }

            let prev_non_space = chars[..start]
                .iter()
                .enumerate()
                .rev()
                .find(|(idx, ch)| mask.get(*idx).copied().unwrap_or(false) && !ch.is_whitespace())
                .map(|(_, ch)| *ch);
            if prev_non_space == Some('>') || prev_non_space == Some(':') {
                continue;
            }

            let before = chars[..start].iter().collect::<String>();
            let before_trimmed = before.trim_end();
            if before_trimmed.ends_with("function")
                || before_trimmed.ends_with("new")
                || before_trimmed.ends_with("if")
                || before_trimmed.ends_with("while")
                || before_trimmed.ends_with("for")
                || before_trimmed.ends_with("switch")
                || before_trimmed.ends_with("catch")
            {
                continue;
            }

            if defined_functions.contains(&lower_name) {
                continue;
            }

            diagnostics.push(Diagnostic {
                range: Range::new(
                    Position::new(line_idx as u32, start as u32),
                    Position::new(line_idx as u32, i as u32),
                ),
                severity: Some(DiagnosticSeverity::WARNING),
                message: format!("Undefined function: {name}()"),
                source: Some("vscode-ls-php".to_string()),
                ..Diagnostic::default()
            });
        }
    }

    diagnostics
}

pub(crate) fn detect_unused_imports(text: &str) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let mut in_block_comment = false;

    for (line_idx, line) in text.lines().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let mask = code_mask_for_line(&chars, &mut in_block_comment);
        let sanitized = chars
            .iter()
            .enumerate()
            .map(|(idx, ch)| if mask.get(idx).copied().unwrap_or(false) { *ch } else { ' ' })
            .collect::<String>();

        let trimmed = sanitized.trim_start();
        if !trimmed.starts_with("use ") || trimmed.contains("use (") || trimmed.contains("use(") {
            continue;
        }

        let mut rest = trimmed.trim_start_matches("use ").trim();
        if rest.starts_with("function ") || rest.starts_with("const ") {
            continue;
        }
        if rest.contains('{') || rest.contains(',') {
            // Keep this pass conservative and skip grouped or multi-import declarations.
            continue;
        }

        if let Some(semi) = rest.find(';') {
            rest = &rest[..semi];
        }

        let mut aliases = HashMap::new();
        parse_single_use_entry(rest, &mut aliases);
        let Some((alias, _)) = aliases.into_iter().next() else {
            continue;
        };

        if alias.is_empty() {
            continue;
        }

        let used_elsewhere = find_identifier_ranges(text, &alias)
            .into_iter()
            .any(|range| range.start.line != line_idx as u32);

        if used_elsewhere {
            continue;
        }

        let Some(column) = sanitized.find(&alias) else {
            continue;
        };
        diagnostics.push(Diagnostic {
            range: Range::new(
                Position::new(line_idx as u32, column as u32),
                Position::new(line_idx as u32, (column + alias.len()) as u32),
            ),
            severity: Some(DiagnosticSeverity::HINT),
            message: format!("Unused import: {alias}"),
            source: Some("vscode-ls-php".to_string()),
            ..Diagnostic::default()
        });
    }

    diagnostics
}

pub(crate) fn detect_duplicate_imports(text: &str) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let mut in_block_comment = false;
    let mut seen_imports: HashMap<String, u32> = HashMap::new();

    for (line_idx, line) in text.lines().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let mask = code_mask_for_line(&chars, &mut in_block_comment);
        let sanitized = chars
            .iter()
            .enumerate()
            .map(|(idx, ch)| if mask.get(idx).copied().unwrap_or(false) { *ch } else { ' ' })
            .collect::<String>();

        let trimmed = sanitized.trim_start();
        if !trimmed.starts_with("use ") || trimmed.contains("use (") || trimmed.contains("use(") {
            continue;
        }

        let mut rest = trimmed.trim_start_matches("use ").trim();
        if rest.starts_with("function ") || rest.starts_with("const ") {
            continue;
        }
        if rest.contains('{') || rest.contains(',') {
            // Keep this pass conservative and skip grouped or multi-import declarations.
            continue;
        }

        if let Some(semi) = rest.find(';') {
            rest = &rest[..semi];
        }

        let mut aliases = HashMap::new();
        parse_single_use_entry(rest, &mut aliases);
        let Some((alias, fqn)) = aliases.into_iter().next() else {
            continue;
        };
        if alias.is_empty() || fqn.is_empty() {
            continue;
        }

        let key = format!("{}=>{}", alias.to_ascii_lowercase(), fqn.to_ascii_lowercase());
        if seen_imports.insert(key, line_idx as u32).is_some() {
            let Some(column) = sanitized.find(&alias) else {
                continue;
            };
            diagnostics.push(Diagnostic {
                range: Range::new(
                    Position::new(line_idx as u32, column as u32),
                    Position::new(line_idx as u32, (column + alias.len()) as u32),
                ),
                severity: Some(DiagnosticSeverity::HINT),
                message: format!("Duplicate import: {alias}"),
                source: Some("vscode-ls-php".to_string()),
                ..Diagnostic::default()
            });
        }
    }

    diagnostics
}

pub(crate) fn detect_missing_return_types(text: &str) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for symbol in extract_symbols(text) {
        if symbol.kind != SymbolKind::FUNCTION {
            continue;
        }
        if symbol.return_type.is_some() {
            continue;
        }
        if symbol.name.is_empty() {
            continue;
        }
        if matches!(symbol.name.as_str(), "__construct" | "__destruct") {
            // Constructors/destructors conventionally omit meaningful return values.
            continue;
        }

        diagnostics.push(Diagnostic {
            range: symbol.range,
            severity: Some(DiagnosticSeverity::HINT),
            message: format!("Missing return type: {}()", symbol.name),
            source: Some("vscode-ls-php".to_string()),
            ..Diagnostic::default()
        });
    }

    diagnostics
}

#[derive(Clone)]
struct ClassMethodScope {
    class_name: String,
    start_line: usize,
    end_line: usize,
    methods: HashSet<String>,
    has_magic_call: bool,
}

pub(crate) fn detect_undefined_methods(text: &str) -> Vec<Diagnostic> {
    let scopes = collect_class_method_scopes(text);
    if scopes.is_empty() {
        return Vec::new();
    }

    let mut diagnostics = Vec::new();
    let mut in_block_comment = false;
    for (line_idx, line) in text.lines().enumerate() {
        let Some(scope) = scopes
            .iter()
            .find(|scope| line_idx >= scope.start_line && line_idx <= scope.end_line)
        else {
            continue;
        };

        if scope.has_magic_call {
            continue;
        }

        let chars: Vec<char> = line.chars().collect();
        let mask = code_mask_for_line(&chars, &mut in_block_comment);
        let mut i = 0usize;
        while i + 3 < chars.len() {
            if !mask.get(i).copied().unwrap_or(false)
                || chars[i] != '$'
                || chars.get(i + 1) != Some(&'t')
                || chars.get(i + 2) != Some(&'h')
                || chars.get(i + 3) != Some(&'i')
                || chars.get(i + 4) != Some(&'s')
            {
                i += 1;
                continue;
            }

            let mut j = i + 5;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j + 1 >= chars.len() || chars[j] != '-' || chars[j + 1] != '>' {
                i += 1;
                continue;
            }

            j += 2;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j >= chars.len() || chars[j] == '$' || !chars[j].is_ascii_alphabetic() && chars[j] != '_' {
                i += 1;
                continue;
            }

            let start = j;
            j += 1;
            while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '_') {
                j += 1;
            }

            let mut k = j;
            while k < chars.len() && chars[k].is_whitespace() {
                k += 1;
            }
            if k >= chars.len() || chars[k] != '(' {
                i = j;
                continue;
            }

            let method_name: String = chars[start..j].iter().collect();
            if scope.methods.contains(&method_name.to_ascii_lowercase()) {
                i = j;
                continue;
            }

            diagnostics.push(Diagnostic {
                range: Range::new(
                    Position::new(line_idx as u32, start as u32),
                    Position::new(line_idx as u32, j as u32),
                ),
                severity: Some(DiagnosticSeverity::WARNING),
                message: format!("Undefined method: {}::{}()", scope.class_name, method_name),
                source: Some("vscode-ls-php".to_string()),
                ..Diagnostic::default()
            });
            i = j;
        }
    }

    diagnostics
}

pub(crate) fn detect_deprecated_usages(text: &str) -> Vec<Diagnostic> {
    let (deprecated_functions, deprecated_types) = collect_deprecated_declarations(text);
    if deprecated_functions.is_empty() && deprecated_types.is_empty() {
        return Vec::new();
    }

    let lines = text.lines().collect::<Vec<_>>();
    let mut diagnostics = Vec::new();
    let mut in_block_comment = false;

    for (line_idx, line) in lines.iter().enumerate() {
        let chars = line.chars().collect::<Vec<_>>();
        let mask = code_mask_for_line(&chars, &mut in_block_comment);

        let mut i = 0usize;
        while i < chars.len() {
            if !mask.get(i).copied().unwrap_or(false)
                || (!chars[i].is_ascii_alphabetic() && chars[i] != '_')
            {
                i += 1;
                continue;
            }

            let start = i;
            i += 1;
            while i < chars.len()
                && mask.get(i).copied().unwrap_or(false)
                && (chars[i].is_ascii_alphanumeric() || chars[i] == '_')
            {
                i += 1;
            }

            let name = chars[start..i].iter().collect::<String>();
            let lowered = name.to_ascii_lowercase();
            let prev_non_space = chars[..start]
                .iter()
                .enumerate()
                .rev()
                .find(|(idx, ch)| mask.get(*idx).copied().unwrap_or(false) && !ch.is_whitespace())
                .map(|(_, ch)| *ch);

            let mut j = i;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }

            let is_call = j < chars.len() && chars[j] == '(';
            if is_call {
                if matches!(prev_non_space, Some('$' | '&')) {
                    continue;
                }
                if let Some(decl_line) = deprecated_functions.get(&lowered) {
                    if *decl_line != line_idx
                        && prev_non_space != Some('>')
                        && prev_non_space != Some(':')
                    {
                        diagnostics.push(Diagnostic {
                            range: Range::new(
                                Position::new(line_idx as u32, start as u32),
                                Position::new(line_idx as u32, i as u32),
                            ),
                            severity: Some(DiagnosticSeverity::WARNING),
                            message: format!("Deprecated symbol used: {}", name),
                            source: Some("vscode-ls-php".to_string()),
                            ..Diagnostic::default()
                        });
                    }
                }
                continue;
            }

            let is_new_expression = lowered == "new";
            if is_new_expression {
                let mut k = i;
                while k < chars.len() && chars[k].is_whitespace() {
                    k += 1;
                }
                if k < chars.len() && chars[k] == '\\' {
                    k += 1;
                }
                if k >= chars.len() || (!chars[k].is_ascii_alphabetic() && chars[k] != '_') {
                    continue;
                }

                let type_start = k;
                k += 1;
                while k < chars.len() && (chars[k].is_ascii_alphanumeric() || chars[k] == '_' || chars[k] == '\\') {
                    k += 1;
                }
                let type_name = chars[type_start..k].iter().collect::<String>();
                let short = type_name.rsplit('\\').next().unwrap_or(type_name.as_str());
                let short_lower = short.to_ascii_lowercase();

                if let Some(decl_line) = deprecated_types.get(&short_lower) {
                    if *decl_line != line_idx {
                        diagnostics.push(Diagnostic {
                            range: Range::new(
                                Position::new(line_idx as u32, type_start as u32),
                                Position::new(line_idx as u32, k as u32),
                            ),
                            severity: Some(DiagnosticSeverity::WARNING),
                            message: format!("Deprecated symbol used: {}", short),
                            source: Some("vscode-ls-php".to_string()),
                            ..Diagnostic::default()
                        });
                    }
                }
            }
        }
    }

    diagnostics
}

fn collect_deprecated_declarations(text: &str) -> (HashMap<String, usize>, HashMap<String, usize>) {
    let mut deprecated_functions = HashMap::new();
    let mut deprecated_types = HashMap::new();
    let lines = text.lines().collect::<Vec<_>>();

    for (line_idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if let Some(name) = token_after_keyword(trimmed, "function") {
            if let Some(docblock) = get_nearby_docblock(lines.as_slice(), line_idx) {
                if docblock.to_ascii_lowercase().contains("@deprecated") {
                    deprecated_functions.insert(name.to_ascii_lowercase(), line_idx);
                }
            }
            continue;
        }

        for keyword in ["class", "interface", "trait", "enum"] {
            if let Some(name) = token_after_keyword(trimmed, keyword) {
                if let Some(docblock) = get_nearby_docblock(lines.as_slice(), line_idx) {
                    if docblock.to_ascii_lowercase().contains("@deprecated") {
                        deprecated_types.insert(name.to_ascii_lowercase(), line_idx);
                    }
                }
            }
        }
    }

    (deprecated_functions, deprecated_types)
}

fn get_nearby_docblock(lines: &[&str], declaration_line_idx: usize) -> Option<String> {
    const MAX_DOCBLOCK_LOOKBACK: usize = 10;
    if declaration_line_idx == 0 {
        return None;
    }

    let mut idx = declaration_line_idx as isize - 1;
    let min_idx = declaration_line_idx.saturating_sub(MAX_DOCBLOCK_LOOKBACK) as isize;

    while idx >= min_idx && lines[idx as usize].trim().is_empty() {
        idx -= 1;
    }
    if idx < min_idx {
        return None;
    }

    if !lines[idx as usize].trim_end().ends_with("*/") {
        return None;
    }

    let mut parts = Vec::new();
    while idx >= min_idx {
        let line = lines[idx as usize].trim();
        parts.push(line.to_string());
        if line.starts_with("/**") {
            parts.reverse();
            return Some(parts.join("\n"));
        }
        idx -= 1;
    }

    None
}

fn collect_class_method_scopes(text: &str) -> Vec<ClassMethodScope> {
    let lines = text.lines().collect::<Vec<_>>();
    let mut scopes = Vec::new();
    let mut in_block_comment = false;
    let mut brace_depth = 0i32;
    let mut pending_class: Option<(String, usize, i32)> = None;

    for (line_idx, line) in lines.iter().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let mask = code_mask_for_line(&chars, &mut in_block_comment);
        let sanitized = chars
            .iter()
            .enumerate()
            .map(|(idx, ch)| if mask.get(idx).copied().unwrap_or(false) { *ch } else { ' ' })
            .collect::<String>();
        let trimmed = sanitized.trim_start();

        if pending_class.is_none() {
            if let Some(name) = token_after_keyword(trimmed, "class") {
                let has_open = trimmed.contains('{');
                if has_open {
                    let open_count = trimmed.chars().filter(|ch| *ch == '{').count() as i32;
                    pending_class = Some((name, line_idx, brace_depth + open_count));
                } else {
                    pending_class = Some((name, line_idx, brace_depth + 1));
                }
            }
        }

        let open_count = sanitized.chars().filter(|ch| *ch == '{').count() as i32;
        let close_count = sanitized.chars().filter(|ch| *ch == '}').count() as i32;
        let after_depth = (brace_depth + open_count - close_count).max(0);

        if let Some((class_name, start_line, start_depth)) = &pending_class {
            if after_depth < *start_depth {
                let mut methods = HashSet::new();
                let mut has_magic_call = false;
                for body_idx in *start_line..=line_idx {
                    let body_line = lines.get(body_idx).copied().unwrap_or_default().trim_start();
                    if let Some(method_name) = token_after_keyword(body_line, "function") {
                        if method_name.eq_ignore_ascii_case("__call") {
                            has_magic_call = true;
                        }
                        methods.insert(method_name.to_ascii_lowercase());
                    }
                }
                scopes.push(ClassMethodScope {
                    class_name: class_name.clone(),
                    start_line: *start_line,
                    end_line: line_idx,
                    methods,
                    has_magic_call,
                });
                pending_class = None;
            }
        }

        brace_depth = after_depth;
    }

    scopes
}

fn condition_segments_for_keyword(line: &str, keyword: &str) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let keyword_chars: Vec<char> = keyword.chars().collect();

    let mut i = 0usize;
    while i + keyword_chars.len() <= chars.len() {
        let is_match = chars[i..(i + keyword_chars.len())] == keyword_chars[..];
        if !is_match {
            i += 1;
            continue;
        }

        let before_ok = i == 0 || !chars[i - 1].is_ascii_alphanumeric();
        let mut j = i + keyword_chars.len();
        while j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        let after_ok = j < chars.len() && chars[j] == '(';

        if !before_ok || !after_ok {
            i += 1;
            continue;
        }

        let Some(close) = matching_paren_index(line, j) else {
            break;
        };

        let segment = line[(j + 1)..close].to_string();
        out.push((segment, (j + 1) as u32));
        i = close + 1;
    }

    out
}

fn matching_paren_index(line: &str, open_idx: usize) -> Option<usize> {
    let chars: Vec<char> = line.chars().collect();
    if chars.get(open_idx).copied() != Some('(') {
        return None;
    }

    let mut depth = 0i32;
    for (idx, ch) in chars.iter().enumerate().skip(open_idx) {
        if *ch == '(' {
            depth += 1;
        } else if *ch == ')' {
            depth -= 1;
            if depth == 0 {
                return Some(idx);
            }
        }
    }

    None
}

fn suspicious_assignment_offset(condition: &str) -> Option<usize> {
    let chars: Vec<char> = condition.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] != '=' {
            i += 1;
            continue;
        }

        let prev = if i == 0 { None } else { Some(chars[i - 1]) };
        let next = chars.get(i + 1).copied();

        if prev == Some('=')
            || prev == Some('!')
            || prev == Some('>')
            || prev == Some('<')
            || prev == Some('?')
            || prev == Some('.')
            || prev == Some('*')
            || prev == Some('&')
            || prev == Some('|')
            || prev == Some('^')
            || prev == Some('+')
            || prev == Some('-')
            || prev == Some('/')
            || prev == Some('%')
            || next == Some('=')
            || next == Some('>')
        {
            i += 1;
            continue;
        }

        return Some(i);
    }

    None
}

pub(crate) fn variable_occurrences_in_line(chars: &[char], code_mask: &[bool]) -> Vec<(String, usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0usize;

    while i < chars.len() {
        if chars[i] != '$' || !code_mask.get(i).copied().unwrap_or(false) {
            i += 1;
            continue;
        }

        let start = i;
        i += 1;
        while i < chars.len()
            && (chars[i].is_ascii_alphanumeric() || chars[i] == '_')
            && code_mask.get(i).copied().unwrap_or(false)
        {
            i += 1;
        }

        if i > start + 1 {
            let name: String = chars[start..i].iter().collect();
            out.push((name, start, i));
        }
    }

    out
}

fn is_assignment_declaration(chars: &[char], end_exclusive: usize, code_mask: &[bool]) -> bool {
    let mut i = end_exclusive;
    while i < chars.len() {
        if !code_mask.get(i).copied().unwrap_or(false) {
            i += 1;
            continue;
        }
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }

        if chars[i] != '=' {
            return false;
        }

        if i + 1 < chars.len() {
            let next = chars[i + 1];
            if next == '=' || next == '>' {
                return false;
            }
        }

        return true;
    }

    false
}

pub(crate) fn extract_first_variable_name(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] != '$' {
            i += 1;
            continue;
        }

        let start = i;
        i += 1;
        while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
            i += 1;
        }

        if i > start + 1 {
            let name: String = chars[start..i].iter().collect();
            return Some(name);
        }
    }

    None
}

fn extract_var_annotation_variables(comment_text: &str) -> Vec<String> {
    let mut vars = Vec::new();
    let patterns = ["@var", "@psalm-var", "@phpstan-var"];

    for pattern in patterns {
        let mut search_start = 0usize;
        while let Some(offset) = comment_text[search_start..].find(pattern) {
            let at = search_start + offset;
            let tail = &comment_text[(at + pattern.len())..];
            if let Some(name) = extract_first_variable_name(tail) {
                vars.push(name);
            }
            search_start = at + pattern.len();
        }
    }

    vars
}

fn extract_foreach_declared_variables(line: &str) -> Vec<String> {
    let mut vars = Vec::new();
    let Some(as_idx) = line.find(" as ") else {
        return vars;
    };

    let tail = &line[(as_idx + 4)..];
    if let Some(var) = extract_first_variable_name(tail) {
        vars.push(var);
    }

    if let Some(arrow_idx) = tail.find("=>") {
        if let Some(var) = extract_first_variable_name(&tail[(arrow_idx + 2)..]) {
            vars.push(var);
        }
    }

    vars
}

fn extract_declared_variables_after_keyword(line: &str, keyword: &str) -> Vec<String> {
    let mut vars = Vec::new();
    let needle = format!("{keyword} ");
    let Some(idx) = line.find(&needle) else {
        return vars;
    };

    let tail = &line[(idx + needle.len())..];
    for part in tail.split(|ch| [',', ';', '{', ')'].contains(&ch)) {
        if let Some(name) = extract_first_variable_name(part) {
            vars.push(name);
        }
    }

    vars
}

fn extract_closure_use_variables(line: &str) -> Vec<String> {
    let mut vars = Vec::new();
    let Some(use_idx) = line.find("use (") else {
        return vars;
    };
    let tail = &line[(use_idx + 5)..];
    let Some(close_idx) = tail.find(')') else {
        return vars;
    };

    let inside = &tail[..close_idx];
    for part in inside.split(',') {
        if let Some(name) = extract_first_variable_name(part) {
            vars.push(name);
        }
    }

    vars
}

pub(crate) fn is_builtin_variable(name: &str) -> bool {
    matches!(
        name,
        "$this"
            | "$GLOBALS"
            | "$_SERVER"
            | "$_GET"
            | "$_POST"
            | "$_FILES"
            | "$_COOKIE"
            | "$_SESSION"
            | "$_REQUEST"
            | "$_ENV"
            | "$http_response_header"
            | "$php_errormsg"
            | "$argc"
            | "$argv"
    )
}
