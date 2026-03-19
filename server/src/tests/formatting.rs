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


