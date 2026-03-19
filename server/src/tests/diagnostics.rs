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


