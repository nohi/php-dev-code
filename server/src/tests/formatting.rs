use super::*;

#[test]
fn range_formatting_preserves_outside_columns_for_partial_selection() {
	let text = "<?php\n  $x = 1;   \n\n\n  $y = 2;   \nreturn $x + $y;\n";
	let requested = Range::new(Position::new(1, 2), Position::new(4, 4));

	let edit = format_range_line_edit(text, requested).expect("edit");
	assert_eq!(edit.range.start, Position::new(1, 2));
	assert_eq!(edit.range.end, Position::new(4, 4));
	assert_eq!(edit.new_text, "  $x = 1;\n\n\n  $y = 2;   ");
}

#[test]
fn current_line_formatting_still_formats_whole_line() {
	let text = "<?php\n$x = 1;   \n$y = 2;\n";
	let edit = format_current_line_edit(text, 1).expect("edit");
	assert_eq!(edit.range.start, Position::new(1, 0));
	assert_eq!(edit.new_text, "$x = 1;");
}

#[test]
fn phpdoc_generator_generates_param_and_return_for_typed_function() {
    let text = "<?php\n\nclass Foo {\n    /**\n    public function bar(string $name, int $count): bool {\n    }\n}\n";
    let result = generate_phpdoc_on_enter(text, 3);
    assert!(result.is_some());
    let edits = result.unwrap();
    assert!(!edits.is_empty());
    let edit_text = &edits[0].new_text;
    assert!(edit_text.contains("@param string $name"), "Expected @param string $name in: {}", edit_text);
    assert!(edit_text.contains("@param int $count"), "Expected @param int $count in: {}", edit_text);
    assert!(edit_text.contains("@return bool"), "Expected @return bool in: {}", edit_text);
}

#[test]
fn phpdoc_generator_no_trigger_for_non_docblock() {
    let text = "<?php\n// regular comment\nfunction foo() {}\n";
    let result = generate_phpdoc_on_enter(text, 1);
    assert!(result.is_none());
}

