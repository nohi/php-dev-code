use super::*;

#[test]
fn diagnostics_detects_undefined_variable_in_split_module() {
	let source = "<?php\n$defined = 1;\n$used = $defined + $missing;";
	let diagnostics = detect_undefined_variables(source);

	assert!(diagnostics.iter().any(|d| d.message == "Undefined variable: $missing"));
}

#[test]
fn diagnostics_detects_brace_mismatch_in_split_module() {
	let source = "<?php\nif ($x) {\n  echo $x;\n";
	let diagnostics = detect_brace_mismatch(source);
	assert!(diagnostics
		.iter()
		.any(|d| d.message == "Unclosed opening brace '{'"));
}

#[test]
fn diagnostics_detects_deprecated_function_usage_in_split_module() {
	let source = "<?php
/** @deprecated use new_api */
function old_api() {}

function run_it() {
	old_api();
}
";
	let diagnostics = detect_deprecated_usages(source);
	assert!(diagnostics
		.iter()
		.any(|d| d.message == "Deprecated symbol used: old_api"));
}

#[test]
fn diagnostics_detects_deprecated_class_instantiation_in_split_module() {
	let source = "<?php
/** @deprecated */
class OldService {}

function make() {
	$svc = new OldService();
}
";
	let diagnostics = detect_deprecated_usages(source);
	assert!(diagnostics
		.iter()
		.any(|d| d.message == "Deprecated symbol used: OldService"));
}

#[test]
fn diagnostics_skips_variable_function_calls_for_deprecated_detection() {
	let source = "<?php
/** @deprecated */
function old_api() {}

$fn = 'old_api';
$fn();
";
	let diagnostics = detect_deprecated_usages(source);
	assert!(!diagnostics
		.iter()
		.any(|d| d.message.contains("Deprecated symbol used:")));
}

#[test]
fn diagnostics_detects_php_version_compatibility_issues() {
	let source = "<?php
	$user?->name;
	$result = match ($x) { default => 1 };
	enum Status { case Ok; }
";
	let diagnostics = detect_php_version_compatibility(source, (7, 4));
	assert!(diagnostics
		.iter()
		.any(|d| d.message.contains("nullsafe operator ?->") && d.message.contains("target is 7.4")));
	assert!(diagnostics
		.iter()
		.any(|d| d.message.contains("match expression") && d.message.contains("target is 7.4")));
	assert!(diagnostics
		.iter()
		.any(|d| d.message.contains("enum declaration") && d.message.contains("target is 7.4")));
}

#[test]
fn diagnostics_allows_features_on_supported_php_target() {
	let source = "<?php
	$user?->name;
	$result = match ($x) { default => 1 };
	enum Status { case Ok; }
	readonly class Profile {}
";
	let diagnostics = detect_php_version_compatibility(source, (8, 2));
	assert!(diagnostics.is_empty());
}

#[test]
fn diagnostics_reports_correct_columns_for_indented_php_version_features() {
	let source = "<?php
    $result = match ($x) { default => 1 };
    enum Status { case Ok; }
";
	let diagnostics = detect_php_version_compatibility(source, (7, 4));

	let match_diag = diagnostics
		.iter()
		.find(|d| d.message.contains("match expression"))
		.expect("match diagnostic");
	assert_eq!(match_diag.range.start.character, 14);

	let enum_diag = diagnostics
		.iter()
		.find(|d| d.message.contains("enum declaration"))
		.expect("enum diagnostic");
	assert_eq!(enum_diag.range.start.character, 4);
}

#[test]
fn diagnostics_detects_comment_task_markers_in_line_and_doc_comments() {
	let source = "<?php
// TODO: remove before merge
/** FIXME update docs */
";
	let diagnostics = detect_comment_task_markers(source);
	assert!(diagnostics
		.iter()
		.any(|d| d.message == "Comment task marker: TODO"));
	assert!(diagnostics
		.iter()
		.any(|d| d.message == "Comment task marker: FIXME"));
}

#[test]
fn diagnostics_ignores_task_markers_outside_comments() {
	let source = "<?php
$note = 'TODO: keep this in string';
echo \"FIXME\";
";
	let diagnostics = detect_comment_task_markers(source);
	assert!(diagnostics.is_empty());
}


