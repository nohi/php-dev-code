mod diagnostics;
mod formatting;

use super::{
        arg_has_content,
        collect_php_files, completion_context_kind, completion_score, extract_local_variables_before_position,
        detect_undefined_variables,
        extract_symbols, find_identifier_ranges, identifier_and_range_at_position,
        identifier_at_position, is_blade_uri, is_code_position, is_php_uri, is_valid_identifier_name,
        import_action_for_fqn,
        parse_namespace_declaration, parse_single_use_entry, parse_use_aliases,
        parse_diagnostic_filter_config_text,
        parse_php_target_version,
        path_matches_directory_rule,
        is_diagnostic_enabled_for_path,
        collect_folding_ranges,
        collect_parameter_inlay_hints_for_range,
        collect_return_type_inlay_hints_for_range,
        build_function_parameter_map,
        collect_class_implementation_locations,
        detect_brace_mismatch,
        detect_unused_variables,
        detect_unused_imports,
        detect_duplicate_imports,
        detect_missing_return_types,
        detect_undefined_function_calls,
        detect_undefined_function_calls_with_known,
        detect_operator_confusion,
        detect_comment_task_markers,
        detect_php_version_compatibility,
        detect_deprecated_usages,
        extract_laravel_route_names_from_text,
        extract_php_array_keys,
        extract_blade_section_names_from_text,
        extract_phpstorm_meta_override_function_names,
        laravel_string_completion_context,
        blade_section_string_completion_context,
        looks_like_blade_template,
        reference_count_title,
        reference_locations_for_symbol,
        selection_ranges_for_positions,
        resolve_symbol_queries, search_terms_for_target_in_document, should_skip_dir,
        split_fqn, split_use_entries, symbol_completion_label, symbol_matches_query,
        find_use_insertion_position, looks_like_type_name, parse_function_parameters,
        parse_function_return_type,
        php_magic_constants,
        symbol_display_name_with_templates,
        extract_class_relationship_targets,
        detect_http_urls,
        document_end_position,
        format_document,
        format_blade_directive_spacing,
        format_range_line_edit,
        format_current_line_edit,
        format_range_text,
        generate_phpdoc_on_enter,
        call_ranges_for_name,
        find_symbol_location_for_queries,
        find_type_definition_in_index,
        function_call_context,
        format_symbol_for_hover,
        format_symbol_for_hover_with_templates,
        extract_mixin_types_from_docblock,
        extract_template_params_from_docblock,
        is_type_symbol_kind,
        php_manual_function_url,
        resolve_completion_item,
        workspace_symbol_matches_query,
        workspace_symbol_score,
        function_keyword_column,
        static_member_completion_context,
        instance_member_completion_context,
        infer_variable_class_before_position,
        infer_variable_type_annotation_before_position,
        parse_generic_type_instance,
        apply_template_substitution,
        collect_class_member_labels,
        collect_class_member_entries,
        extract_promoted_property_names,
        extract_promoted_property_names_from_lines,
        extract_promoted_property_entries_from_lines,
        delete_line_range,
        tokenize_php_document,
        compress_tokens_to_semantic_data,
        ParsedSemanticToken,
        brace_mismatch_fix_action,
        missing_return_type_add_action,
        php_tag_insert_action,
        duplicate_import_remove_action,
        unused_import_remove_action,
        unused_variable_remove_action,
        operator_confusion_compare_action,
        undefined_var_declare_action,
        var_dump_delete_action,
        Backend,
        CompletionContextKind, NamespaceDeclaration, PhpSymbol, SymbolKind,
    };
    use std::collections::{BTreeMap, HashMap, HashSet};
    use std::sync::{Arc, Mutex};
    use std::path::Path;
    use tokio::sync::RwLock;
    use serde_json::json;
    use tower_lsp::{LanguageServer, LspService};
    use tower_lsp::lsp_types::{
        CodeActionOrCommand, CompletionItem, CompletionItemKind, CompletionParams, Diagnostic, DiagnosticSeverity, InlayHintLabel,
        Position, Range,
        TextDocumentIdentifier, TextDocumentPositionParams,
        Url,
    };

    #[test]
    fn extracts_basic_php_symbols() {
        let source = "<?php\nclass Demo {}\nfunction run_test() {}\nconst VERSION = '1';";
        let symbols = extract_symbols(source);

        assert!(symbols.iter().any(|s| s.name == "Demo" && s.kind == SymbolKind::CLASS));
        assert!(symbols.iter().any(|s| s.name == "run_test" && s.kind == SymbolKind::FUNCTION));
        assert!(symbols.iter().any(|s| s.name == "VERSION" && s.kind == SymbolKind::CONSTANT));
    }

    #[test]
    fn parses_diagnostic_filter_config_text_with_rules_and_paths() {
        let cfg = parse_diagnostic_filter_config_text(
            r#"{
  "diagnostics": {
    "disableRules": ["unused-import"],
    "disableInPaths": {
      "undefined-variable": ["tests/**", "vendor/"]
    }
  }
}"#,
        );

        assert!(cfg.disabled_rules.contains("unused-import"));
        assert_eq!(
            cfg.disabled_in_paths
                .get("undefined-variable")
                .cloned()
                .unwrap_or_default(),
            vec!["tests/**".to_string(), "vendor/".to_string()]
        );
    }

    #[test]
    fn matches_directory_rules_with_and_without_glob_suffix() {
        assert!(path_matches_directory_rule("tests/Feature/UserTest.php", "tests/**"));
        assert!(path_matches_directory_rule("vendor/laravel/framework.php", "vendor/"));
        assert!(!path_matches_directory_rule("app/Http/Controller.php", "tests/**"));
    }

    #[test]
    fn disables_diagnostics_by_rule_and_path() {
        let cfg = parse_diagnostic_filter_config_text(
            r#"{
  "diagnostics": {
    "disableRules": ["unused-import"],
    "disableInPaths": {
      "undefined-variable": ["tests/**"]
    }
  }
}"#,
        );

        let undefined_var = Diagnostic {
            range: Range::new(Position::new(0, 0), Position::new(0, 2)),
            severity: Some(DiagnosticSeverity::WARNING),
            message: "Undefined variable: $x".to_string(),
            source: Some("vscode-ls-php".to_string()),
            ..Diagnostic::default()
        };
        let unused_import = Diagnostic {
            range: Range::new(Position::new(0, 0), Position::new(0, 3)),
            severity: Some(DiagnosticSeverity::HINT),
            message: "Unused import: User".to_string(),
            source: Some("vscode-ls-php".to_string()),
            ..Diagnostic::default()
        };
                let todo_comment = Diagnostic {
                        range: Range::new(Position::new(0, 0), Position::new(0, 4)),
                        severity: Some(DiagnosticSeverity::HINT),
                        message: "Comment task marker: TODO".to_string(),
                        source: Some("vscode-ls-php".to_string()),
                        ..Diagnostic::default()
                };

                let cfg_with_todo_rule = parse_diagnostic_filter_config_text(
                        r#"{
    "diagnostics": {
        "disableRules": ["todo-comment"]
    }
}"#,
                );

        assert!(!is_diagnostic_enabled_for_path(
            &undefined_var,
            Some("tests/Feature/UserTest.php"),
            &cfg
        ));
        assert!(is_diagnostic_enabled_for_path(
            &undefined_var,
            Some("app/Http/Controller.php"),
            &cfg
        ));
        assert!(!is_diagnostic_enabled_for_path(
            &unused_import,
            Some("app/Http/Controller.php"),
            &cfg
        ));
        assert!(!is_diagnostic_enabled_for_path(
            &todo_comment,
            Some("app/Http/Controller.php"),
            &cfg_with_todo_rule
        ));
    }

    #[test]
    fn parses_php_target_version_from_diagnostic_config() {
        let cfg = parse_diagnostic_filter_config_text(
            r#"{
  "diagnostics": {
    "phpTargetVersion": "7.4"
  }
}"#,
        );
        assert_eq!(cfg.php_target_version, Some((7, 4)));
        assert_eq!(parse_php_target_version("8.1"), Some((8, 1)));
        assert!(parse_php_target_version("8").is_none());
    }

    #[test]
    fn extracts_namespace_for_symbols() {
        let source = "<?php\nnamespace App\\Models;\nclass User {}";
        let symbols = extract_symbols(source);
        let user = symbols.iter().find(|s| s.name == "User").expect("user symbol");
        assert_eq!(user.namespace.as_deref(), Some("App\\Models"));
    }

    #[test]
    fn supports_block_namespace_scope() {
        let source = "<?php\nnamespace App\\Models {\nclass User {}\n}\nclass GlobalUser {}";
        let symbols = extract_symbols(source);

        let user = symbols.iter().find(|s| s.name == "User").expect("user symbol");
        assert_eq!(user.namespace.as_deref(), Some("App\\Models"));

        let global_user = symbols
            .iter()
            .find(|s| s.name == "GlobalUser")
            .expect("global symbol");
        assert_eq!(global_user.namespace, None);
    }

    #[test]
    fn detects_identifier_at_cursor_position() {
        let source = "<?php\nfunction run_test() {}\nrun_test();";
        let position = Position::new(2, 3);
        let ident = identifier_at_position(source, position);
        assert_eq!(ident.as_deref(), Some("run_test"));
    }

    #[test]
    fn extracts_identifier_and_range_for_variable() {
        let source = "<?php\n$sample_name = 1;";
        let position = Position::new(1, 3);
        let result = identifier_and_range_at_position(source, position);

        let Some((ident, range)) = result else {
            panic!("expected identifier");
        };

        assert_eq!(ident, "$sample_name");
        assert_eq!(range.start.character, 0);
    }

    #[test]
    fn finds_identifier_occurrences_with_boundaries() {
        let source = "<?php\nrun_test();\nrun_test;\nrun_test_extra();";
        let ranges = find_identifier_ranges(source, "run_test");
        assert_eq!(ranges.len(), 2);
    }

    #[test]
    fn ignores_occurrences_in_comments_and_strings() {
        let source = "<?php\nrun_test();\n// run_test\n# run_test\n/* run_test */\n$text = \"run_test\";\nrun_test;";
        let ranges = find_identifier_ranges(source, "run_test");
        assert_eq!(ranges.len(), 2);
    }

    #[test]
    fn highlights_variable_occurrences_in_code_only() {
        let source = "<?php\n$value = 1;\n$value += 2;\n// $value\n$text = \"$value\";";
        let ranges = find_identifier_ranges(source, "$value");
        assert_eq!(ranges.len(), 2);
    }

    #[test]
    fn validates_php_identifier_names() {
        assert!(is_valid_identifier_name("RenamedValue"));
        assert!(is_valid_identifier_name("$renamed_value"));
        assert!(!is_valid_identifier_name(""));
        assert!(!is_valid_identifier_name("123abc"));
        assert!(!is_valid_identifier_name("invalid-name"));
    }

    #[test]
    fn rejects_comment_and_string_positions_as_code() {
        let source = "<?php\nrun_test();\n// run_test\n$text = \"run_test\";";
        assert!(is_code_position(source, Position::new(1, 2)));
        assert!(!is_code_position(source, Position::new(2, 4)));
        assert!(!is_code_position(source, Position::new(3, 10)));
    }

    #[test]
    fn skips_common_large_directories() {
        assert!(should_skip_dir(Path::new("vendor")));
        assert!(should_skip_dir(Path::new("node_modules")));
        assert!(should_skip_dir(Path::new(".git")));
        assert!(!should_skip_dir(Path::new("app")));
    }

    #[test]
    fn collects_php_files_recursively() {
        let root = std::env::temp_dir().join("vscode_ls_php_collect_files_test");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).expect("create src dir");
        std::fs::create_dir_all(root.join("vendor")).expect("create vendor dir");
        std::fs::write(root.join("src").join("a.php"), "<?php").expect("write php");
        std::fs::write(root.join("src").join("b.txt"), "x").expect("write txt");
        std::fs::write(root.join("vendor").join("c.php"), "<?php").expect("write vendor php");

        let mut out = Vec::new();
        collect_php_files(&root, &mut out);

        assert_eq!(out.len(), 1);
        assert!(out[0].ends_with(Path::new("src").join("a.php")));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn detects_php_uri_by_extension() {
        let php = Url::parse("file:///tmp/test.php").expect("url");
        let blade = Url::parse("file:///tmp/index.blade.php").expect("url");
        let txt = Url::parse("file:///tmp/test.txt").expect("url");
        assert!(is_php_uri(&php));
        assert!(is_php_uri(&blade));
        assert!(is_blade_uri(&blade));
        assert!(!is_php_uri(&txt));
    }

    #[test]
    fn detects_laravel_route_name_literals() {
        let source = "<?php\nRoute::get('/home', HomeController::class)->name('home');\nRoute::post('/login', AuthController::class)->name(\"auth.login\");\n";
        let names = extract_laravel_route_names_from_text(source);
        assert!(names.contains("home"));
        assert!(names.contains("auth.login"));
    }

    #[test]
    fn extracts_php_array_keys_for_config_candidates() {
        let source = "<?php\nreturn [\n  'name' => 'demo',\n  'timezone' => 'UTC',\n  'nested' => [\n    'driver' => 'redis'\n  ],\n];\n";
        let keys = extract_php_array_keys(source);
        assert!(keys.contains("name"));
        assert!(keys.contains("timezone"));
        assert!(keys.contains("nested"));
        assert!(keys.contains("driver"));
    }

    #[test]
    fn detects_route_and_config_string_completion_context() {
        let route_source = "<?php\nroute('home');\n";
        let route_ctx = laravel_string_completion_context(route_source, Position::new(1, 10), "route");
        assert_eq!(route_ctx, Some('\''));

        let config_source = "<?php\nconfig(\"app.name\");\n";
        let config_ctx = laravel_string_completion_context(config_source, Position::new(1, 12), "config");
        assert_eq!(config_ctx, Some('"'));
    }

    #[test]
    fn detects_blade_section_string_completion_context() {
        let source = "@extends('layouts.app')\n@section('cont')\n";
        let section_ctx = blade_section_string_completion_context(source, Position::new(1, 14));
        assert_eq!(section_ctx, Some('\''));

        let source2 = "@yield(\"header\")\n";
        let yield_ctx = blade_section_string_completion_context(source2, Position::new(0, 11));
        assert_eq!(yield_ctx, Some('"'));
    }

    #[test]
    fn extracts_blade_section_names_from_yield_and_section() {
        let source = "@yield('content')\n@section(\"header\")\n";
        let names = extract_blade_section_names_from_text(source);
        assert!(names.contains("content"));
        assert!(names.contains("header"));
    }

    #[test]
    fn detects_blade_template_heuristics() {
        let blade = "@extends('layouts.app')\n@section('content')\n{{ $slot }}\n@endsection\n";
        assert!(looks_like_blade_template(blade));

        let php = "<?php\nfunction run() { return 1; }\n";
        assert!(!looks_like_blade_template(php));
    }

    #[test]
    fn formats_blade_directive_spacing() {
        assert_eq!(format_blade_directive_spacing("@if($a)"), "@if ($a)");
        assert_eq!(format_blade_directive_spacing("@foreach($items as $item)"), "@foreach ($items as $item)");
    }

    #[test]
    fn format_document_applies_blade_directive_spacing() {
        let source = "@if($ready)\n<div>ok</div>\n@endif\n";
        let formatted = format_document(source);
        assert!(formatted.starts_with("@if ($ready)\n"));
    }

    #[test]
    fn tokenizes_blade_directive_as_semantic_function_token() {
        let source = "@if ($ok)\n<div>{{ $ok }}</div>\n@endif\n";
        let tokens = tokenize_php_document(source);
        assert!(tokens.iter().any(|t| t.line == 0 && t.char == 0 && t.token_type == 1));
    }

    #[test]
    fn tokenizes_html_tags_in_template() {
        let source = "<section>\n  <div class=\"x\"></div>\n</section>\n";
        let tokens = tokenize_php_document(source);
        assert!(tokens.iter().any(|t| t.line == 0 && t.char == 1 && t.token_type == 5));
        assert!(tokens.iter().any(|t| t.line == 1 && t.char == 3 && t.token_type == 5));
    }

    #[test]
    fn tokenizes_embedded_js_and_css_keywords_in_template() {
        let source = "<script>\nconst value = 1;\nfunction run() { return value; }\n</script>\n<style>\ncolor: red;\nmargin-top: 4px;\n</style>\n";
        let tokens = tokenize_php_document(source);
        assert!(tokens.iter().any(|t| t.line == 1 && t.token_type == 1));
        assert!(tokens.iter().any(|t| t.line == 2 && t.token_type == 1));
        assert!(tokens.iter().any(|t| t.line == 5 && t.token_type == 0));
        assert!(tokens.iter().any(|t| t.line == 6 && t.token_type == 0));
    }

    #[test]
    fn scores_local_function_higher_than_keyword() {
        let function_score = completion_score(
            "run_test",
            "run",
            true,
            Some(CompletionItemKind::FUNCTION),
            CompletionContextKind::General,
        );
        let keyword_score = completion_score(
            "return",
            "re",
            false,
            Some(CompletionItemKind::KEYWORD),
            CompletionContextKind::General,
        );
        assert!(function_score > keyword_score);
    }

    #[test]
    fn includes_magic_constants_in_completion_keywords() {
        let constants = php_magic_constants();
        assert!(constants.contains(&"__FILE__"));
        assert!(constants.contains(&"__DIR__"));
        assert!(constants.contains(&"__NAMESPACE__"));
    }

    #[test]
    fn detects_use_statement_completion_context() {
        let source = "<?php\nuse App\\";
        let context = completion_context_kind(source, Position::new(1, 8));
        assert!(matches!(context, CompletionContextKind::UseStatement));
    }

    #[test]
    fn parses_namespace_declaration() {
        let ns = parse_namespace_declaration("namespace App\\Domain\\Auth;");
        assert!(matches!(
            ns,
            Some(NamespaceDeclaration::Inline(value)) if value == "App\\Domain\\Auth"
        ));

        let block = parse_namespace_declaration("namespace App\\Domain { ");
        assert!(matches!(
            block,
            Some(NamespaceDeclaration::Block(value)) if value == "App\\Domain"
        ));

        let global = parse_namespace_declaration("namespace;");
        assert!(matches!(global, Some(NamespaceDeclaration::Global)));
    }

    #[test]
    fn uses_fqn_label_in_use_context() {
        let source = "<?php\nnamespace App\\Models;\nclass User {}";
        let symbols = extract_symbols(source);
        let user = symbols.iter().find(|s| s.name == "User").expect("user symbol");

        let label = symbol_completion_label(user, CompletionContextKind::UseStatement);
        assert_eq!(label, "App\\Models\\User");
    }

    #[test]
    fn parses_use_aliases_and_resolves_alias_query() {
        let source = "<?php\nnamespace App\\Http\\Controllers;\nuse App\\Models\\User as AppUser;\n";
        let aliases = parse_use_aliases(source);
        assert_eq!(aliases.get("AppUser"), Some(&"App\\Models\\User".to_string()));

        let queries = resolve_symbol_queries(source, Position::new(2, 1), "AppUser");
        assert_eq!(queries.first(), Some(&"App\\Models\\User".to_string()));
        assert!(queries.contains(&"App\\Models\\User".to_string()));
    }

    #[test]
    fn ignores_closure_use_syntax_when_parsing_aliases() {
        let source = "<?php\n$fn = function() use($user) { return $user; };\nuse App\\Models\\User;";
        let aliases = parse_use_aliases(source);
        assert_eq!(aliases.get("User"), Some(&"App\\Models\\User".to_string()));
        assert_eq!(aliases.len(), 1);
    }

    #[test]
    fn parses_group_use_aliases() {
        let source = "<?php\nuse App\\Models\\{User, Post as BlogPost};\n";
        let aliases = parse_use_aliases(source);
        assert_eq!(aliases.get("User"), Some(&"App\\Models\\User".to_string()));
        assert_eq!(aliases.get("BlogPost"), Some(&"App\\Models\\Post".to_string()));
    }

    #[test]
    fn splits_use_entries_at_top_level_commas() {
        let entries = split_use_entries("App\\A, App\\B\\{C, D as E}, App\\F");
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[1], "App\\B\\{C, D as E}");
    }

    #[test]
    fn parses_single_use_entry_alias() {
        let mut aliases = HashMap::new();
        parse_single_use_entry("App\\Support\\Str as StringHelper", &mut aliases);
        assert_eq!(
            aliases.get("StringHelper"),
            Some(&"App\\Support\\Str".to_string())
        );
    }

    #[test]
    fn builds_search_terms_for_target_from_document_context() {
        let source = "<?php\nnamespace App\\Http;\nuse App\\Models\\User as AppUser;\n";
        let terms = search_terms_for_target_in_document("App\\Models\\User", source, true);
        assert!(terms.contains(&"AppUser".to_string()));
        assert!(terms.contains(&"App\\Models\\User".to_string()));
        assert!(terms.contains(&"\\App\\Models\\User".to_string()));
    }

    #[test]
    fn builds_search_terms_for_group_use_aliases() {
        let source = "<?php\nuse App\\Models\\{User as AppUser, Post};\n";

        let terms_for_refs = search_terms_for_target_in_document("App\\Models\\User", source, true);
        assert!(terms_for_refs.contains(&"AppUser".to_string()));

        let terms_for_rename = search_terms_for_target_in_document("App\\Models\\User", source, false);
        assert!(!terms_for_rename.contains(&"AppUser".to_string()));
        assert!(terms_for_rename.contains(&"App\\Models\\User".to_string()));
    }

    #[test]
    fn splits_fqn_into_namespace_and_short_name() {
        let (ns, short) = split_fqn("App\\Models\\User");
        assert_eq!(ns.as_deref(), Some("App\\Models"));
        assert_eq!(short, "User");
    }

    #[test]
    fn detects_type_like_names() {
        assert!(looks_like_type_name("User"));
        assert!(looks_like_type_name("_InternalType"));
        assert!(!looks_like_type_name("user"));
        assert!(!looks_like_type_name("$User"));
    }

    #[test]
    fn computes_use_insertion_position() {
        let source = "<?php\nnamespace App\\Http;\nuse App\\Models\\Post;\nclass C {}";
        let pos = find_use_insertion_position(source);
        assert_eq!(pos.line, 3);
        assert_eq!(pos.character, 0);
    }

    #[test]
    fn matches_symbol_with_fqn_query() {
        let symbol = PhpSymbol {
            name: "User".to_string(),
            namespace: Some("App\\Models".to_string()),
            kind: SymbolKind::CLASS,
            parameters: Vec::new(),
            return_type: None,
            range: Range::new(Position::new(0, 0), Position::new(0, 1)),
        };

        assert!(symbol_matches_query(&symbol, "App\\Models\\User"));
        assert!(symbol_matches_query(&symbol, "User"));
        assert!(!symbol_matches_query(&symbol, "App\\Models\\Order"));
    }

    #[test]
    fn recognizes_type_symbol_kinds() {
        assert!(is_type_symbol_kind(SymbolKind::CLASS));
        assert!(is_type_symbol_kind(SymbolKind::INTERFACE));
        assert!(is_type_symbol_kind(SymbolKind::ENUM));
        assert!(!is_type_symbol_kind(SymbolKind::FUNCTION));
    }

    #[test]
    fn finds_type_definition_from_current_document() {
        let current_uri = Url::parse("file:///tmp/current.php").expect("current uri");
        let index = HashMap::from([(
            current_uri.clone(),
            vec![PhpSymbol {
                name: "User".to_string(),
                namespace: Some("App\\Models".to_string()),
                kind: SymbolKind::CLASS,
                parameters: Vec::new(),
                return_type: None,
                range: Range::new(Position::new(2, 0), Position::new(2, 4)),
            }],
        )]);

        let location = find_type_definition_in_index(
            &index,
            &current_uri,
            &["App\\Models\\User".to_string()],
        )
        .expect("location");
        assert_eq!(location.uri, current_uri);
        assert_eq!(location.range.start, Position::new(2, 0));
    }

    #[test]
    fn skips_non_type_symbols_when_finding_type_definition() {
        let current_uri = Url::parse("file:///tmp/current.php").expect("current uri");
        let external_uri = Url::parse("file:///tmp/external.php").expect("external uri");
        let index = HashMap::from([
            (
                current_uri.clone(),
                vec![PhpSymbol {
                    name: "User".to_string(),
                    namespace: Some("App\\Models".to_string()),
                    kind: SymbolKind::FUNCTION,
                    parameters: Vec::new(),
                    return_type: None,
                    range: Range::new(Position::new(1, 0), Position::new(1, 4)),
                }],
            ),
            (
                external_uri.clone(),
                vec![PhpSymbol {
                    name: "User".to_string(),
                    namespace: Some("App\\Models".to_string()),
                    kind: SymbolKind::CLASS,
                    parameters: Vec::new(),
                    return_type: None,
                    range: Range::new(Position::new(6, 0), Position::new(6, 4)),
                }],
            ),
        ]);

        let location =
            find_type_definition_in_index(&index, &current_uri, &["User".to_string()]).expect("location");
        assert_eq!(location.uri, external_uri);
        assert_eq!(location.range.start, Position::new(6, 0));
    }

    #[test]
    fn prefers_current_document_type_definition() {
        let current_uri = Url::parse("file:///tmp/current.php").expect("current uri");
        let external_uri = Url::parse("file:///tmp/external.php").expect("external uri");
        let index = HashMap::from([
            (
                current_uri.clone(),
                vec![PhpSymbol {
                    name: "User".to_string(),
                    namespace: Some("App\\Models".to_string()),
                    kind: SymbolKind::CLASS,
                    parameters: Vec::new(),
                    return_type: None,
                    range: Range::new(Position::new(3, 0), Position::new(3, 4)),
                }],
            ),
            (
                external_uri,
                vec![PhpSymbol {
                    name: "User".to_string(),
                    namespace: Some("App\\Models".to_string()),
                    kind: SymbolKind::CLASS,
                    parameters: Vec::new(),
                    return_type: None,
                    range: Range::new(Position::new(8, 0), Position::new(8, 4)),
                }],
            ),
        ]);

        let location =
            find_type_definition_in_index(&index, &current_uri, &["User".to_string()]).expect("location");
        assert_eq!(location.uri, current_uri);
        assert_eq!(location.range.start, Position::new(3, 0));
    }

    #[test]
    fn returns_none_when_type_definition_not_found() {
        let current_uri = Url::parse("file:///tmp/current.php").expect("current uri");
        let index = HashMap::from([(
            current_uri.clone(),
            vec![PhpSymbol {
                name: "run".to_string(),
                namespace: None,
                kind: SymbolKind::FUNCTION,
                parameters: Vec::new(),
                return_type: None,
                range: Range::new(Position::new(1, 0), Position::new(1, 3)),
            }],
        )]);

        let location = find_type_definition_in_index(&index, &current_uri, &["User".to_string()]);
        assert!(location.is_none());
    }

    #[test]
    fn matches_workspace_symbol_query_by_fqn_and_kind_tokens() {
        let symbol = PhpSymbol {
            name: "User".to_string(),
            namespace: Some("App\\Models".to_string()),
            kind: SymbolKind::CLASS,
            parameters: Vec::new(),
            return_type: None,
            range: Range::new(Position::new(0, 0), Position::new(0, 4)),
        };

        assert!(workspace_symbol_matches_query(&symbol, "app\\models class"));
        assert!(workspace_symbol_matches_query(&symbol, "user"));
        assert!(!workspace_symbol_matches_query(&symbol, "service interface"));
    }

    #[test]
    fn matches_workspace_symbol_query_with_empty_input() {
        let symbol = PhpSymbol {
            name: "run".to_string(),
            namespace: None,
            kind: SymbolKind::FUNCTION,
            parameters: Vec::new(),
            return_type: None,
            range: Range::new(Position::new(0, 0), Position::new(0, 3)),
        };

        assert!(workspace_symbol_matches_query(&symbol, ""));
        assert!(workspace_symbol_matches_query(&symbol, "   "));
    }

    #[test]
    fn scores_workspace_symbol_higher_for_exact_name_match() {
        let exact = PhpSymbol {
            name: "User".to_string(),
            namespace: Some("App\\Models".to_string()),
            kind: SymbolKind::CLASS,
            parameters: Vec::new(),
            return_type: None,
            range: Range::new(Position::new(0, 0), Position::new(0, 4)),
        };
        let prefix = PhpSymbol {
            name: "UserRepository".to_string(),
            namespace: Some("App\\Models".to_string()),
            kind: SymbolKind::CLASS,
            parameters: Vec::new(),
            return_type: None,
            range: Range::new(Position::new(0, 0), Position::new(0, 13)),
        };

        let exact_score = workspace_symbol_score(&exact, "user").expect("exact score");
        let prefix_score = workspace_symbol_score(&prefix, "user").expect("prefix score");
        assert!(exact_score > prefix_score);
    }

    #[test]
    fn skips_workspace_symbol_score_when_query_not_matched() {
        let symbol = PhpSymbol {
            name: "User".to_string(),
            namespace: Some("App\\Models".to_string()),
            kind: SymbolKind::CLASS,
            parameters: Vec::new(),
            return_type: None,
            range: Range::new(Position::new(0, 0), Position::new(0, 4)),
        };

        assert!(workspace_symbol_score(&symbol, "invoice").is_none());
    }

    #[test]
    fn detects_call_ranges_for_function_calls_only() {
        let source = "<?php\nrun_test($a);\n$run_test = 1;\nrun_test\n";
        let ranges = call_ranges_for_name(source, "run_test");
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].start, Position::new(1, 0));
    }

    #[test]
    fn detects_static_member_completion_context() {
        let source = "<?php\nUser::get";
        let context = static_member_completion_context(source, Position::new(1, 9));
        assert_eq!(context.as_deref(), Some("User"));
    }

    #[test]
    fn detects_namespaced_static_member_completion_context() {
        let source = "<?php\nApp\\Models\\User::get";
        let context = static_member_completion_context(source, Position::new(1, 20));
        assert_eq!(context.as_deref(), Some("App\\Models\\User"));
    }

    #[test]
    fn detects_instance_member_completion_context() {
        let source = "<?php\n$user->get";
        let context = instance_member_completion_context(source, Position::new(1, 10));
        assert_eq!(context.as_deref(), Some("$user"));
    }

    #[test]
    fn infers_variable_class_before_position_from_new_assignment() {
        let source = "<?php\n$user = new App\\Models\\User();\n$user->get";
        let inferred = infer_variable_class_before_position(source, Position::new(2, 10), "$user");
        assert_eq!(inferred.as_deref(), Some("App\\Models\\User"));
    }

    #[test]
    fn skips_variable_class_inference_for_comparison_operator() {
        let source = "<?php\nif ($user == new App\\Models\\User()) {}\n$user->get";
        let inferred = infer_variable_class_before_position(source, Position::new(2, 10), "$user");
        assert!(inferred.is_none());
    }

    #[test]
    fn skips_variable_class_inference_inside_string_or_comment() {
        let source = "<?php\n// $user = new App\\Models\\Ghost();\n$txt = \"$user = new App\\Models\\Ghost()\";\n$user = new App\\Models\\User();\n$user->get";
        let inferred = infer_variable_class_before_position(source, Position::new(4, 10), "$user");
        assert_eq!(inferred.as_deref(), Some("App\\Models\\User"));
    }

    #[test]
    fn skips_variable_class_inference_for_assignments_after_cursor_on_same_line() {
        let source = "<?php\n$user->get(); $user = new App\\Models\\User();";
        let inferred = infer_variable_class_before_position(source, Position::new(1, 10), "$user");
        assert!(inferred.is_none());
    }

    #[test]
    fn infers_variable_class_before_position_with_uppercase_new_keyword() {
        let source = "<?php\n$user = NEW App\\Models\\User();\n$user->get";
        let inferred = infer_variable_class_before_position(source, Position::new(2, 10), "$user");
        assert_eq!(inferred.as_deref(), Some("App\\Models\\User"));
    }

    #[test]
    fn infers_variable_class_before_position_from_generic_var_annotation() {
        let source = "<?php\n/** @var Collection<int, App\\Models\\User> $users */\n$users->first();";
        let inferred = infer_variable_class_before_position(source, Position::new(2, 11), "$users");
        assert_eq!(inferred.as_deref(), Some("Collection"));
    }

    #[test]
    fn infers_variable_class_before_position_from_nullable_union_var_annotation() {
        let source = "<?php\n/** @var ?App\\Models\\User|null $user */\n$user->getId();";
        let inferred = infer_variable_class_before_position(source, Position::new(2, 10), "$user");
        assert_eq!(inferred.as_deref(), Some("App\\Models\\User"));
    }

    #[test]
    fn skips_var_annotation_inference_inside_string_literal() {
        let source = "<?php\n$txt = \"@var App\\Models\\Ghost $user\";\n$user->getId();";
        let inferred = infer_variable_class_before_position(source, Position::new(2, 10), "$user");
        assert!(inferred.is_none());
    }

    #[test]
    fn infers_variable_raw_type_annotation_before_position() {
        let source = "<?php\n/** @var Repository<App\\Models\\User> $repo */\n$repo->find();";
        let inferred = infer_variable_type_annotation_before_position(source, Position::new(2, 10), "$repo");
        assert_eq!(inferred.as_deref(), Some("Repository<App\\Models\\User>"));
    }

    #[test]
    fn parses_generic_type_instance_arguments() {
        let parsed = parse_generic_type_instance("Repository<App\\Models\\User, int>").expect("parsed generic");
        assert_eq!(parsed.0, "Repository");
        assert_eq!(parsed.1, vec!["App\\Models\\User", "int"]);
    }

    #[test]
    fn parses_nullable_generic_type_instance() {
        let parsed = parse_generic_type_instance("?Repository<App\\Models\\User>").expect("parsed generic");
        assert_eq!(parsed.0, "Repository");
        assert_eq!(parsed.1, vec!["App\\Models\\User"]);
    }

    #[test]
    fn parses_union_generic_type_instance() {
        let parsed = parse_generic_type_instance("Repository<App\\Models\\User>|null").expect("parsed generic");
        assert_eq!(parsed.0, "Repository");
        assert_eq!(parsed.1, vec!["App\\Models\\User"]);
    }

    #[test]
    fn parses_nested_generic_type_instance_arguments() {
        let parsed = parse_generic_type_instance("Repository<Iterator<App\\Models\\User>, int>").expect("parsed generic");
        assert_eq!(parsed.0, "Repository");
        assert_eq!(parsed.1, vec!["Iterator<App\\Models\\User>", "int"]);
    }

    #[test]
    fn parses_namespaced_generic_type_instance() {
        let parsed = parse_generic_type_instance("\\App\\Models\\Repository<App\\Models\\User>").expect("parsed generic");
        assert_eq!(parsed.0, "App\\Models\\Repository");
        assert_eq!(parsed.1, vec!["App\\Models\\User"]);
    }

    #[test]
    fn rejects_malformed_generic_type_instance() {
        assert!(parse_generic_type_instance("Repository<App\\Models\\User>>").is_none());
    }

    #[test]
    fn applies_template_substitution_to_member_type() {
        let mapping = BTreeMap::from([
            ("TModel".to_string(), "App\\Models\\User".to_string()),
            ("TKey".to_string(), "int".to_string()),
        ]);

        let resolved = apply_template_substitution("Collection<TKey, TModel>|null", &mapping);
        assert_eq!(resolved, "Collection<int, App\\Models\\User>|null");
    }

    #[test]
    fn skips_static_member_completion_context_inside_string() {
        let source = "<?php\n\"User::get\"";
        let context = static_member_completion_context(source, Position::new(1, 10));
        assert!(context.is_none());
    }

    #[test]
    fn skips_static_member_completion_context_inside_comment() {
        let source = "<?php\n// User::get";
        let context = static_member_completion_context(source, Position::new(1, 12));
        assert!(context.is_none());
    }

    #[test]
    fn collects_class_member_labels_for_completion() {
        let text = "<?php\nclass User {\n  public function getName() {}\n  private $email;\n}\n";
        let class_symbol = PhpSymbol {
            name: "User".to_string(),
            namespace: None,
            kind: SymbolKind::CLASS,
            parameters: Vec::new(),
            return_type: None,
            range: Range::new(Position::new(1, 6), Position::new(1, 10)),
        };

        let members = collect_class_member_labels(text, &class_symbol);
        assert!(members.iter().any(|(name, kind)| name == "getName" && *kind == CompletionItemKind::METHOD));
        assert!(members.iter().any(|(name, kind)| name == "email" && *kind == CompletionItemKind::FIELD));
    }

    #[test]
    fn collects_class_member_labels_with_braces_in_strings_and_comments() {
        let text = "<?php\nclass User {\n  public function getName() {\n    $x = \"{not-a-brace}\";\n    // } commented brace\n    return $x;\n  }\n  private $email;\n}\n";
        let class_symbol = PhpSymbol {
            name: "User".to_string(),
            namespace: None,
            kind: SymbolKind::CLASS,
            parameters: Vec::new(),
            return_type: None,
            range: Range::new(Position::new(1, 6), Position::new(1, 10)),
        };

        let members = collect_class_member_labels(text, &class_symbol);
        assert!(members.iter().any(|(name, kind)| name == "getName" && *kind == CompletionItemKind::METHOD));
        assert!(members.iter().any(|(name, kind)| name == "email" && *kind == CompletionItemKind::FIELD));
    }

    #[test]
    fn collects_class_member_labels_after_long_class_declaration() {
        let text = "<?php\nclass User\n  extends BaseUser\n  implements A,\n             B,\n             C\n{\n  public function getId() {}\n}\n";
        let class_symbol = PhpSymbol {
            name: "User".to_string(),
            namespace: None,
            kind: SymbolKind::CLASS,
            parameters: Vec::new(),
            return_type: None,
            range: Range::new(Position::new(1, 6), Position::new(1, 10)),
        };

        let members = collect_class_member_labels(text, &class_symbol);
        assert!(members.iter().any(|(name, kind)| name == "getId" && *kind == CompletionItemKind::METHOD));
    }

    #[test]
    fn collects_multiple_class_properties_on_single_line() {
        let text = "<?php\nclass Config {\n  public $host, $port, $user;\n}\n";
        let class_symbol = PhpSymbol {
            name: "Config".to_string(),
            namespace: None,
            kind: SymbolKind::CLASS,
            parameters: Vec::new(),
            return_type: None,
            range: Range::new(Position::new(1, 6), Position::new(1, 12)),
        };

        let members = collect_class_member_labels(text, &class_symbol);
        assert!(members.iter().any(|(name, kind)| name == "host" && *kind == CompletionItemKind::FIELD));
        assert!(members.iter().any(|(name, kind)| name == "port" && *kind == CompletionItemKind::FIELD));
        assert!(members.iter().any(|(name, kind)| name == "user" && *kind == CompletionItemKind::FIELD));
    }

    #[test]
    fn extracts_promoted_property_names_from_constructor_signature() {
        let names = extract_promoted_property_names(
            "function __construct(public string $email, private int $id, $plain) {}",
        );
        assert_eq!(names, vec!["email".to_string(), "id".to_string()]);
    }

    #[test]
    fn extracts_promoted_property_names_with_protected_visibility() {
        let names = extract_promoted_property_names(
            "function __construct(protected string $token, $plain) {}",
        );
        assert_eq!(names, vec!["token".to_string()]);
    }

    #[test]
    fn collects_promoted_properties_from_constructor_for_completion() {
        let text = "<?php\nclass User {\n  public function __construct(public string $email, private int $id) {}\n}\n";
        let class_symbol = PhpSymbol {
            name: "User".to_string(),
            namespace: None,
            kind: SymbolKind::CLASS,
            parameters: Vec::new(),
            return_type: None,
            range: Range::new(Position::new(1, 6), Position::new(1, 10)),
        };

        let members = collect_class_member_labels(text, &class_symbol);
        assert!(members.iter().any(|(name, kind)| name == "email" && *kind == CompletionItemKind::FIELD));
        assert!(members.iter().any(|(name, kind)| name == "id" && *kind == CompletionItemKind::FIELD));
    }

    #[test]
    fn extracts_promoted_property_names_from_multiline_constructor_signature() {
        let lines = vec![
            "public function __construct(",
            "  public string $email,",
            "  private int $id",
            ") {}",
        ];
        let names = extract_promoted_property_names_from_lines(&lines, 0);
        assert_eq!(names, vec!["email".to_string(), "id".to_string()]);
    }

    #[test]
    fn extracts_promoted_property_type_hints_from_constructor_signature() {
        let lines = vec!["function __construct(public string $email, private ?User $owner, protected Collection<int, User> $users) {}"];
        let entries = extract_promoted_property_entries_from_lines(&lines, 0);

        assert!(entries
            .iter()
            .any(|e| e.name == "email" && e.type_hint.as_deref() == Some("string")));
        assert!(entries
            .iter()
            .any(|e| e.name == "owner" && e.type_hint.as_deref() == Some("?User")));
        assert!(entries
            .iter()
            .any(|e| e.name == "users" && e.type_hint.as_deref() == Some("Collection<int, User>")));
    }

    #[test]
    fn collects_promoted_properties_from_multiline_constructor_for_completion() {
        let text = "<?php\nclass User {\n  public function __construct(\n    public string $email,\n    private int $id\n  ) {}\n}\n";
        let class_symbol = PhpSymbol {
            name: "User".to_string(),
            namespace: None,
            kind: SymbolKind::CLASS,
            parameters: Vec::new(),
            return_type: None,
            range: Range::new(Position::new(1, 6), Position::new(1, 10)),
        };

        let members = collect_class_member_labels(text, &class_symbol);
        assert!(members.iter().any(|(name, kind)| name == "email" && *kind == CompletionItemKind::FIELD));
        assert!(members.iter().any(|(name, kind)| name == "id" && *kind == CompletionItemKind::FIELD));
    }

    #[test]
    fn marks_static_members_in_class_member_entries() {
        let text = "<?php\nclass User {\n  public static function find(): ?User {}\n  public function load(): Collection<int, User> {}\n  private static array $cache;\n  private ?string $email;\n}\n";
        let class_symbol = PhpSymbol {
            name: "User".to_string(),
            namespace: None,
            kind: SymbolKind::CLASS,
            parameters: Vec::new(),
            return_type: None,
            range: Range::new(Position::new(1, 6), Position::new(1, 10)),
        };

        let entries = collect_class_member_entries(text, &class_symbol);
        assert!(entries.iter().any(|e| e.label == "find" && e.is_static));
        assert!(entries.iter().any(|e| e.label == "load" && !e.is_static));
        assert!(entries.iter().any(|e| e.label == "cache" && e.is_static));
        assert!(entries.iter().any(|e| e.label == "email" && !e.is_static));
        assert!(entries
            .iter()
            .any(|e| e.label == "find" && e.type_hint.as_deref() == Some("?User")));
        assert!(entries
            .iter()
            .any(|e| e.label == "load" && e.type_hint.as_deref() == Some("Collection<int, User>")));
        assert!(entries
            .iter()
            .any(|e| e.label == "cache" && e.type_hint.as_deref() == Some("array")));
        assert!(entries
            .iter()
            .any(|e| e.label == "email" && e.type_hint.as_deref() == Some("?string")));
    }

    #[test]
    fn marks_promoted_properties_as_non_static_entries() {
        let text =
            "<?php\nclass User {\n  public function __construct(public string $email, private int $id) {}\n}\n";
        let class_symbol = PhpSymbol {
            name: "User".to_string(),
            namespace: None,
            kind: SymbolKind::CLASS,
            parameters: Vec::new(),
            return_type: None,
            range: Range::new(Position::new(1, 6), Position::new(1, 10)),
        };

        let entries = collect_class_member_entries(text, &class_symbol);
        assert!(entries
            .iter()
            .any(|e| e.label == "email" && !e.is_static && e.type_hint.as_deref() == Some("string")));
        assert!(entries
            .iter()
            .any(|e| e.label == "id" && !e.is_static && e.type_hint.as_deref() == Some("int")));
    }

    #[test]
    fn marks_multiline_promoted_properties_with_type_hints() {
        let text = "<?php\nclass User {\n  public function __construct(\n    public string $email,\n    private ?User $owner\n  ) {}\n}\n";
        let class_symbol = PhpSymbol {
            name: "User".to_string(),
            namespace: None,
            kind: SymbolKind::CLASS,
            parameters: Vec::new(),
            return_type: None,
            range: Range::new(Position::new(1, 6), Position::new(1, 10)),
        };

        let entries = collect_class_member_entries(text, &class_symbol);
        assert!(entries
            .iter()
            .any(|e| e.label == "email" && e.type_hint.as_deref() == Some("string")));
        assert!(entries
            .iter()
            .any(|e| e.label == "owner" && e.type_hint.as_deref() == Some("?User")));
    }

    #[tokio::test]
    async fn completion_response_concretizes_member_type_from_template_argument() {
        let captured_client = Arc::new(Mutex::new(None));
        let captured_client_for_service = Arc::clone(&captured_client);
        let (_service, _socket) = LspService::new(move |client| {
            *captured_client_for_service
                .lock()
                .expect("client mutex") = Some(client.clone());
            Backend {
                client,
                documents: RwLock::new(HashMap::new()),
                symbols: RwLock::new(HashMap::new()),
                workspace_folders: RwLock::new(Vec::new()),
                open_documents: RwLock::new(HashSet::new()),
            }
        });

        let client = captured_client
            .lock()
            .expect("client mutex")
            .take()
            .expect("captured client");
        let backend = Backend {
            client,
            documents: RwLock::new(HashMap::new()),
            symbols: RwLock::new(HashMap::new()),
            workspace_folders: RwLock::new(Vec::new()),
            open_documents: RwLock::new(HashSet::new()),
        };

        let uri = Url::parse("file:///tmp/member-type-generic.php").expect("uri");
        let source = "<?php
    /**
     * @template TModel
     */
    class Repository {
  public function first(): TModel {}
}
/** @var Repository<App\\Models\\User> $repo */
$repo->fi
";
        backend.update_document(uri.clone(), source.to_string()).await;

        let response = backend
            .completion(CompletionParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    position: Position::new(8, 8),
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
                context: None,
            })
            .await
            .expect("completion result")
            .expect("completion response");

        let items = match response {
            tower_lsp::lsp_types::CompletionResponse::Array(items) => items,
            tower_lsp::lsp_types::CompletionResponse::List(list) => list.items,
        };

        let first_item = items
            .iter()
            .find(|item| {
                item.label == "first"
                    && item
                        .data
                        .as_ref()
                        .and_then(|v| v.as_object())
                        .and_then(|o| o.get("memberType"))
                        .and_then(|v| v.as_str())
                        .is_some()
            })
            .expect("first() member completion item");
        assert_eq!(
            first_item.detail.as_deref(),
            Some("Instance member: App\\Models\\User")
        );

        let member_type = first_item
            .data
            .as_ref()
            .and_then(|v| v.as_object())
            .and_then(|o| o.get("memberType"))
            .and_then(|v| v.as_str());
        assert_eq!(member_type, Some("App\\Models\\User"));
    }

    #[tokio::test]
    async fn completion_response_includes_instance_members_from_mixin_docblock() {
        let captured_client = Arc::new(Mutex::new(None));
        let captured_client_for_service = Arc::clone(&captured_client);
        let (_service, _socket) = LspService::new(move |client| {
            *captured_client_for_service
                .lock()
                .expect("client mutex") = Some(client.clone());
            Backend {
                client,
                documents: RwLock::new(HashMap::new()),
                symbols: RwLock::new(HashMap::new()),
                workspace_folders: RwLock::new(Vec::new()),
                open_documents: RwLock::new(HashSet::new()),
            }
        });

        let client = captured_client
            .lock()
            .expect("client mutex")
            .take()
            .expect("captured client");
        let backend = Backend {
            client,
            documents: RwLock::new(HashMap::new()),
            symbols: RwLock::new(HashMap::new()),
            workspace_folders: RwLock::new(Vec::new()),
            open_documents: RwLock::new(HashSet::new()),
        };

        let uri = Url::parse("file:///tmp/mixin-completion.php").expect("uri");
        let source = "<?php
namespace App\\Core;

class MagicMixin {
  public function fromMixin(): string {}
}

/** @mixin App\\Core\\MagicMixin */
class Repository {
}

$repo = new Repository();
$repo->fr
";
        backend.update_document(uri.clone(), source.to_string()).await;

        let response = backend
            .completion(CompletionParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    position: Position::new(12, 8),
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
                context: None,
            })
            .await
            .expect("completion result")
            .expect("completion response");

        let items = match response {
            tower_lsp::lsp_types::CompletionResponse::Array(items) => items,
            tower_lsp::lsp_types::CompletionResponse::List(list) => list.items,
        };

        let mixin_item = items
            .iter()
            .find(|item| {
                item.label == "fromMixin"
                    && item
                        .data
                        .as_ref()
                        .and_then(|v| v.as_object())
                        .and_then(|o| o.get("memberOf"))
                        .and_then(|v| v.as_str())
                        == Some("App\\Core\\MagicMixin")
            })
            .expect("fromMixin member completion item");
        assert_eq!(
            mixin_item.detail.as_deref(),
            Some("Instance member (mixin): string")
        );
        let member_of = mixin_item
            .data
            .as_ref()
            .and_then(|v| v.as_object())
            .and_then(|o| o.get("memberOf"))
            .and_then(|v| v.as_str());
        assert_eq!(member_of, Some("App\\Core\\MagicMixin"));
    }

    #[tokio::test]
    async fn completion_response_adds_auto_import_with_alias_on_conflict() {
        let captured_client = Arc::new(Mutex::new(None));
        let captured_client_for_service = Arc::clone(&captured_client);
        let (_service, _socket) = LspService::new(move |client| {
            *captured_client_for_service
                .lock()
                .expect("client mutex") = Some(client.clone());
            Backend {
                client,
                documents: RwLock::new(HashMap::new()),
                symbols: RwLock::new(HashMap::new()),
                workspace_folders: RwLock::new(Vec::new()),
                open_documents: RwLock::new(HashSet::new()),
            }
        });

        let client = captured_client
            .lock()
            .expect("client mutex")
            .take()
            .expect("captured client");
        let backend = Backend {
            client,
            documents: RwLock::new(HashMap::new()),
            symbols: RwLock::new(HashMap::new()),
            workspace_folders: RwLock::new(Vec::new()),
            open_documents: RwLock::new(HashSet::new()),
        };

        let current_uri = Url::parse("file:///tmp/auto-import-current.php").expect("current uri");
        let current_source = "<?php
namespace App\\Http\\Controllers;
use App\\Models\\User;

class Controller {
  public function run() {
    Us
  }
}
";
        backend
            .update_document(current_uri.clone(), current_source.to_string())
            .await;

        let service_uri = Url::parse("file:///tmp/auto-import-service.php").expect("service uri");
        let service_source = "<?php
namespace App\\Services;

class User {}
";
        backend
            .update_document(service_uri, service_source.to_string())
            .await;

        let response = backend
            .completion(CompletionParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: current_uri.clone() },
                    position: Position::new(6, 6),
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
                context: None,
            })
            .await
            .expect("completion result")
            .expect("completion response");

        let items = match response {
            tower_lsp::lsp_types::CompletionResponse::Array(items) => items,
            tower_lsp::lsp_types::CompletionResponse::List(list) => list.items,
        };

        let target = items
            .iter()
            .find(|item| {
                item.data
                    .as_ref()
                    .and_then(|v| v.as_str())
                    .map(|s| s == "App\\Services\\User")
                    .unwrap_or(false)
            })
            .expect("App\\Services\\User completion item");

        assert_eq!(target.insert_text.as_deref(), Some("UserAlias"));
        let edits = target
            .additional_text_edits
            .as_ref()
            .expect("additional text edits");
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].new_text, "use App\\Services\\User as UserAlias;\n");
    }

    #[tokio::test]
    async fn completion_response_suggests_blade_section_names_between_yield_and_section() {
        let captured_client = Arc::new(Mutex::new(None));
        let captured_client_for_service = Arc::clone(&captured_client);
        let (_service, _socket) = LspService::new(move |client| {
            *captured_client_for_service
                .lock()
                .expect("client mutex") = Some(client.clone());
            Backend {
                client,
                documents: RwLock::new(HashMap::new()),
                symbols: RwLock::new(HashMap::new()),
                workspace_folders: RwLock::new(Vec::new()),
                open_documents: RwLock::new(HashSet::new()),
            }
        });

        let client = captured_client
            .lock()
            .expect("client mutex")
            .take()
            .expect("captured client");
        let backend = Backend {
            client,
            documents: RwLock::new(HashMap::new()),
            symbols: RwLock::new(HashMap::new()),
            workspace_folders: RwLock::new(Vec::new()),
            open_documents: RwLock::new(HashSet::new()),
        };

        let shared_uri = Url::parse("file:///tmp/layout.blade.php").expect("shared uri");
        let shared_source = "@yield('content')\n";
        backend
            .update_document(shared_uri, shared_source.to_string())
            .await;

        let current_uri = Url::parse("file:///tmp/page.blade.php").expect("current uri");
        let current_source = "@extends('layout')\n@section('co')\n";
        backend
            .update_document(current_uri.clone(), current_source.to_string())
            .await;

        let response = backend
            .completion(CompletionParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: current_uri.clone() },
                    position: Position::new(1, 12),
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
                context: None,
            })
            .await
            .expect("completion result")
            .expect("completion response");

        let items = match response {
            tower_lsp::lsp_types::CompletionResponse::Array(items) => items,
            tower_lsp::lsp_types::CompletionResponse::List(list) => list.items,
        };

        let content_item = items
            .iter()
            .find(|item| item.label == "content")
            .expect("content section completion");
        assert_eq!(content_item.detail.as_deref(), Some("Blade section"));
    }

    #[test]
    fn finds_symbol_location_for_queries_prefers_current_document() {
        let current_uri = Url::parse("file:///tmp/current.php").expect("current uri");
        let external_uri = Url::parse("file:///tmp/external.php").expect("external uri");
        let current_symbol = PhpSymbol {
            name: "User".to_string(),
            namespace: Some("App\\Local".to_string()),
            kind: SymbolKind::CLASS,
            parameters: Vec::new(),
            return_type: None,
            range: Range::new(Position::new(2, 0), Position::new(2, 4)),
        };
        let external_symbol = PhpSymbol {
            name: "User".to_string(),
            namespace: Some("App\\External".to_string()),
            kind: SymbolKind::CLASS,
            parameters: Vec::new(),
            return_type: None,
            range: Range::new(Position::new(6, 0), Position::new(6, 4)),
        };

        let index = HashMap::from([
            (current_uri.clone(), vec![current_symbol]),
            (external_uri, vec![external_symbol]),
        ]);
        let queries = vec!["User".to_string()];

        let (uri, symbol) =
            find_symbol_location_for_queries(&current_uri, &index, &queries).expect("symbol");
        assert_eq!(uri, &current_uri);
        assert_eq!(symbol.namespace.as_deref(), Some("App\\Local"));
    }

    #[test]
    fn extracts_local_variables_before_cursor() {
        let source = "<?php\n$first = 1;\n$second = $first + 1;\n";
        let vars = extract_local_variables_before_position(source, Position::new(2, 20));
        assert!(vars.contains(&"$first".to_string()));
        assert!(vars.contains(&"$second".to_string()));
    }

    #[test]
    fn resolves_completion_item_for_workspace_symbol() {
        let uri = Url::parse("file:///tmp/a.php").expect("uri");
        let index = HashMap::from([(
            uri,
            vec![PhpSymbol {
                name: "User".to_string(),
                namespace: Some("App\\Models".to_string()),
                kind: SymbolKind::CLASS,
                parameters: Vec::new(),
                return_type: None,
                range: Range::new(Position::new(2, 0), Position::new(2, 4)),
            }],
        )]);

        let resolved = resolve_completion_item(
            CompletionItem {
                label: "User".to_string(),
                ..CompletionItem::default()
            },
            &index,
        );

        assert_eq!(resolved.detail.as_deref(), Some("Class symbol"));
        match resolved.documentation {
            Some(tower_lsp::lsp_types::Documentation::MarkupContent(content)) => {
                assert!(content.value.contains("**Class** `User`"));
            }
            _ => panic!("expected markdown documentation"),
        }
    }

    #[test]
    fn resolves_completion_item_for_magic_constant() {
        let resolved = resolve_completion_item(
            CompletionItem {
                label: "__FILE__".to_string(),
                ..CompletionItem::default()
            },
            &HashMap::new(),
        );

        assert_eq!(resolved.detail.as_deref(), Some("PHP magic constant"));
        match resolved.documentation {
            Some(tower_lsp::lsp_types::Documentation::String(content)) => {
                assert_eq!(content, "Built-in PHP magic constant.");
            }
            _ => panic!("expected string documentation"),
        }
    }

    #[test]
    fn resolves_completion_item_prefers_data_fqn_for_ambiguous_labels() {
        let uri_a = Url::parse("file:///tmp/a.php").expect("uri a");
        let uri_b = Url::parse("file:///tmp/b.php").expect("uri b");
        let index = HashMap::from([
            (
                uri_a,
                vec![PhpSymbol {
                    name: "User".to_string(),
                    namespace: Some("App\\Models".to_string()),
                    kind: SymbolKind::CLASS,
                    parameters: Vec::new(),
                    return_type: None,
                    range: Range::new(Position::new(1, 0), Position::new(1, 4)),
                }],
            ),
            (
                uri_b,
                vec![PhpSymbol {
                    name: "User".to_string(),
                    namespace: Some("App\\Http".to_string()),
                    kind: SymbolKind::CLASS,
                    parameters: Vec::new(),
                    return_type: None,
                    range: Range::new(Position::new(2, 0), Position::new(2, 4)),
                }],
            ),
        ]);

        let resolved = resolve_completion_item(
            CompletionItem {
                label: "User".to_string(),
                data: Some(json!("App\\Http\\User")),
                ..CompletionItem::default()
            },
            &index,
        );

        match resolved.documentation {
            Some(tower_lsp::lsp_types::Documentation::MarkupContent(content)) => {
                assert!(content.value.contains("**Namespace:** `App\\Http`"));
            }
            _ => panic!("expected markdown documentation"),
        }
    }

    #[test]
    fn resolves_completion_item_for_class_member_metadata() {
        let resolved = resolve_completion_item(
            CompletionItem {
                label: "getName".to_string(),
                data: Some(json!({
                    "memberOf": "App\\Models\\User",
                    "member": "getName",
                    "memberKind": "method"
                })),
                ..CompletionItem::default()
            },
            &HashMap::new(),
        );

        assert_eq!(resolved.detail.as_deref(), Some("method member"));
        match resolved.documentation {
            Some(tower_lsp::lsp_types::Documentation::MarkupContent(content)) => {
                assert!(content.value.contains("**Method** `getName`"));
                assert!(content.value.contains("App\\Models\\User"));
            }
            _ => panic!("expected markdown documentation"),
        }
    }

    #[test]
    fn resolves_completion_item_for_class_member_metadata_with_type() {
        let resolved = resolve_completion_item(
            CompletionItem {
                label: "getName".to_string(),
                data: Some(json!({
                    "memberOf": "App\\Models\\User",
                    "member": "getName",
                    "memberKind": "method",
                    "memberType": "string"
                })),
                ..CompletionItem::default()
            },
            &HashMap::new(),
        );

        assert_eq!(resolved.detail.as_deref(), Some("method member: string"));
        match resolved.documentation {
            Some(tower_lsp::lsp_types::Documentation::MarkupContent(content)) => {
                assert!(content.value.contains("**Type:** `string`"));
            }
            _ => panic!("expected markdown documentation"),
        }
    }

    #[test]
    fn parses_function_parameters() {
        let params = parse_function_parameters("function run(string $name, int $age = 1) {");
        assert_eq!(params.len(), 2);
        assert_eq!(params[0], "string $name");
        assert_eq!(params[1], "int $age = 1");
    }

    #[test]
    fn parses_function_return_types() {
        assert_eq!(
            parse_function_return_type("function test(): void {"),
            Some("void".to_string())
        );
        assert_eq!(
            parse_function_return_type("function test(): ?string {"),
            Some("?string".to_string())
        );
        assert_eq!(
            parse_function_return_type("function test(): Foo\\Bar {"),
            Some("Foo\\Bar".to_string())
        );
        assert_eq!(
            parse_function_return_type("function test(): A|B|null {"),
            Some("A|B|null".to_string())
        );
        assert_eq!(parse_function_return_type("function test() {"), None);
        assert_eq!(
            parse_function_return_type("function test(string $a): int|string {"),
            Some("int|string".to_string())
        );
        assert_eq!(
            parse_function_return_type("function test(): Collection<User>|null {"),
            Some("Collection<User>|null".to_string())
        );
        assert_eq!(
            parse_function_return_type("function test(): A&B {"),
            Some("A&B".to_string())
        );
        assert_eq!(
            parse_function_return_type("function test(): callable(int, string): bool {"),
            Some("callable(int, string): bool".to_string())
        );
        assert_eq!(
            parse_function_return_type("function test( string $name ): Collection<User>|null {"),
            Some("Collection<User>|null".to_string())
        );
    }

    #[test]
    fn extracts_function_return_type_in_symbols() {
        let source =
            "<?php\nfunction getUser(): User { }\nfunction greet(): void { }\nfunction compute(): int|float { }";
        let symbols = extract_symbols(source);

        let get_user = symbols
            .iter()
            .find(|s| s.name == "getUser")
            .expect("getUser symbol");
        assert_eq!(get_user.return_type.as_deref(), Some("User"));

        let greet = symbols
            .iter()
            .find(|s| s.name == "greet")
            .expect("greet symbol");
        assert_eq!(greet.return_type.as_deref(), Some("void"));

        let compute = symbols
            .iter()
            .find(|s| s.name == "compute")
            .expect("compute symbol");
        assert_eq!(compute.return_type.as_deref(), Some("int|float"));
    }

    #[test]
    fn extracts_multiline_function_signature_symbols() {
        let source = "<?php\nfunction loadUsers(\n    string $name,\n    int $age\n): Collection<User>|null {\n    return null;\n}\n";
        let symbols = extract_symbols(source);
        let load_users = symbols
            .iter()
            .find(|s| s.name == "loadUsers")
            .expect("loadUsers symbol");

        assert_eq!(load_users.parameters.len(), 2);
        assert_eq!(load_users.parameters[0], "string $name");
        assert_eq!(load_users.parameters[1], "int $age");
        assert_eq!(load_users.return_type.as_deref(), Some("Collection<User>|null"));
    }

    #[test]
    fn extracts_multiline_signature_beyond_twelve_lines() {
        let source = "<?php\nfunction longSig(\n    int $a1,\n    int $a2,\n    int $a3,\n    int $a4,\n    int $a5,\n    int $a6,\n    int $a7,\n    int $a8,\n    int $a9,\n    int $a10,\n    int $a11,\n    int $a12,\n    int $a13\n): int {\n    return 1;\n}\n";
        let symbols = extract_symbols(source);
        let long_sig = symbols
            .iter()
            .find(|s| s.name == "longSig")
            .expect("longSig symbol");

        assert_eq!(long_sig.parameters.len(), 13);
        assert_eq!(long_sig.return_type.as_deref(), Some("int"));
    }

    #[test]
    fn ignores_comment_braces_while_collecting_multiline_signature() {
        let source = "<?php\nfunction withComment(\n    int $a, // {\n    int $b\n): int {\n    return 1;\n}\n";
        let symbols = extract_symbols(source);
        let with_comment = symbols
            .iter()
            .find(|s| s.name == "withComment")
            .expect("withComment symbol");

        assert_eq!(with_comment.parameters.len(), 2);
        assert_eq!(with_comment.return_type.as_deref(), Some("int"));
    }

    #[test]
    fn falls_back_to_phpdoc_return_type_when_missing_inline_type() {
        let source = "<?php\n/** @return Collection<User>|null */\nfunction loadUsers() { return null; }\n";
        let symbols = extract_symbols(source);

        let load_users = symbols
            .iter()
            .find(|s| s.name == "loadUsers")
            .expect("loadUsers symbol");
        assert_eq!(
            load_users.return_type.as_deref(),
            Some("Collection<User>|null")
        );
    }

    #[test]
    fn falls_back_to_phpdoc_return_type_across_single_blank_line() {
        let source = "<?php\n/** @return User */\n\nfunction loadUser() { return null; }\n";
        let symbols = extract_symbols(source);

        let load_user = symbols
            .iter()
            .find(|s| s.name == "loadUser")
            .expect("loadUser symbol");
        assert_eq!(load_user.return_type.as_deref(), Some("User"));
    }

    #[test]
    fn inline_return_type_overrides_phpdoc_return_type() {
        let source = "<?php\n/** @return string */\nfunction loadCount(): int { return 1; }\n";
        let symbols = extract_symbols(source);

        let load_count = symbols
            .iter()
            .find(|s| s.name == "loadCount")
            .expect("loadCount symbol");
        assert_eq!(load_count.return_type.as_deref(), Some("int"));
    }

    #[test]
    fn does_not_use_docblock_from_unrelated_declaration() {
        let source = "<?php\n/** @return string */\nclass Service {}\nfunction run() { return 1; }\n";
        let symbols = extract_symbols(source);

        let run = symbols
            .iter()
            .find(|s| s.name == "run")
            .expect("run symbol");
        assert_eq!(run.return_type, None);
    }

    #[test]
    fn does_not_use_docblock_beyond_max_lookback() {
        let source = "<?php\n/** @return User */\n\n\n\n\n\n\n\n\n\n\n\nfunction loadUser() { return null; }\n";
        let symbols = extract_symbols(source);

        let load_user = symbols
            .iter()
            .find(|s| s.name == "loadUser")
            .expect("loadUser symbol");
        assert_eq!(load_user.return_type, None);
    }

    #[test]
    fn falls_back_to_phpdoc_return_type_across_multiple_blank_lines() {
        let source = "<?php\n/** @return User */\n\n\nfunction loadUserAgain() { return null; }\n";
        let symbols = extract_symbols(source);

        let load_user_again = symbols
            .iter()
            .find(|s| s.name == "loadUserAgain")
            .expect("loadUserAgain symbol");
        assert_eq!(load_user_again.return_type.as_deref(), Some("User"));
    }

    #[test]
    fn falls_back_to_psalm_return_type_when_missing_inline_type() {
        let source = "<?php\n/** @psalm-return array<int, User> */\nfunction loadUsers() { return []; }\n";
        let symbols = extract_symbols(source);

        let load_users = symbols
            .iter()
            .find(|s| s.name == "loadUsers")
            .expect("loadUsers symbol");
        assert_eq!(load_users.return_type.as_deref(), Some("array<int, User>"));
    }

    #[test]
    fn falls_back_to_phpstan_return_type_when_missing_inline_type() {
        let source =
            "<?php\n/** @phpstan-return Collection<User>|null */\nfunction loadUsers() { return null; }\n";
        let symbols = extract_symbols(source);

        let load_users = symbols
            .iter()
            .find(|s| s.name == "loadUsers")
            .expect("loadUsers symbol");
        assert_eq!(
            load_users.return_type.as_deref(),
            Some("Collection<User>|null")
        );
    }

    #[test]
    fn prefers_psalm_return_over_generic_return_when_both_present() {
        let source = "<?php\n/** @return string @psalm-return array<int, User> */\nfunction loadUsers() { return []; }\n";
        let symbols = extract_symbols(source);

        let load_users = symbols
            .iter()
            .find(|s| s.name == "loadUsers")
            .expect("loadUsers symbol");
        assert_eq!(load_users.return_type.as_deref(), Some("array<int, User>"));
    }

    #[test]
    fn extracts_phpstorm_meta_override_function_names() {
        let source = "<?php\nnamespace PHPSTORM_META {\noverride(\\App\\Meta\\resolve(), map(['x' => '@']));\noverride(\\strlen(0), map(['x' => '@']));\noverride(App\\Meta\\build(), map(['x' => '@']));\noverride(\\App\\Meta\\Resolver::make(), map(['x' => '@']));\n}\n";

        let names = extract_phpstorm_meta_override_function_names(source);
        assert!(names.contains("app\\meta\\resolve"));
        assert!(names.contains("app\\meta\\build"));
        assert!(!names.contains("strlen"));
        assert!(!names.contains("app\\meta\\resolver::make"));
    }

    #[test]
    fn extracts_phpstorm_meta_override_function_names_from_multiline_calls() {
        let source = "<?php\nnamespace PHPSTORM_META {\noverride(\n    \\App\\Meta\\resolve(),\n    map(['x' => '@'])\n);\n}\n";

        let names = extract_phpstorm_meta_override_function_names(source);
        assert!(names.contains("app\\meta\\resolve"));
    }

    #[test]
    fn detects_function_call_context() {
        let source = "<?php\nrun_test($a, $b, );";
        let (name, active) = function_call_context(source, Position::new(1, 15)).expect("context");
        assert_eq!(name, "run_test");
        assert_eq!(active, 1);
    }

    #[test]
    fn detects_function_call_context_after_trailing_comma() {
        let source = "<?php\nrun_test($a, $b, );";
        let (name, active) = function_call_context(source, Position::new(1, 17)).expect("context");
        assert_eq!(name, "run_test");
        assert_eq!(active, 2);
    }

    #[test]
    fn detects_undefined_variable_access() {
        let source = "<?php\n$defined = 1;\n$used = $defined + $missing;";
        let diagnostics = detect_undefined_variables(source);

        assert!(diagnostics.iter().any(|d| d.message == "Undefined variable: $missing"));
        assert!(!diagnostics.iter().any(|d| d.message == "Undefined variable: $defined"));
    }

    #[test]
    fn detects_unused_variable_in_function_scope() {
        let source = "<?php\nfunction run() {\n  $unused = 1;\n  return 1;\n}\n";
        let diagnostics = detect_unused_variables(source);

        assert!(diagnostics
            .iter()
            .any(|d| d.message == "Unused variable: $unused"));
    }

    #[test]
    fn skips_used_variable_in_function_scope() {
        let source = "<?php\nfunction run() {\n  $value = 1;\n  return $value;\n}\n";
        let diagnostics = detect_unused_variables(source);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn skips_global_variable_for_unused_check() {
        let source = "<?php\n$unusedGlobal = 1;\n";
        let diagnostics = detect_unused_variables(source);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn detects_unused_simple_import() {
        let source = "<?php\nuse App\\Models\\User;\nclass A {}\n";
        let diagnostics = detect_unused_imports(source);
        assert!(diagnostics
            .iter()
            .any(|d| d.message == "Unused import: User"));
    }

    #[test]
    fn skips_used_simple_import() {
        let source = "<?php\nuse App\\Models\\User;\nfunction run(User $u) {}\n";
        let diagnostics = detect_unused_imports(source);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn skips_group_imports_for_safety() {
        let source = "<?php\nuse App\\Models\\{User, Post};\n";
        let diagnostics = detect_unused_imports(source);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn detects_duplicate_simple_import() {
        let source = "<?php\nuse App\\Models\\User;\nuse App\\Models\\User;\n";
        let diagnostics = detect_duplicate_imports(source);
        assert!(diagnostics
            .iter()
            .any(|d| d.message == "Duplicate import: User"));
    }

    #[test]
    fn skips_non_duplicate_imports() {
        let source = "<?php\nuse App\\Models\\User;\nuse App\\Models\\Post;\n";
        let diagnostics = detect_duplicate_imports(source);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn skips_group_imports_for_duplicate_check() {
        let source = "<?php\nuse App\\Models\\{User, Post};\nuse App\\Models\\{User, Post};\n";
        let diagnostics = detect_duplicate_imports(source);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn detects_duplicate_import_with_explicit_alias() {
        let source = "<?php\nuse App\\Models\\User as U;\nuse App\\Models\\User as U;\n";
        let diagnostics = detect_duplicate_imports(source);
        assert!(diagnostics
            .iter()
            .any(|d| d.message == "Duplicate import: U"));
    }

    #[test]
    fn skips_same_alias_for_different_import_targets() {
        let source = "<?php\nuse App\\Models\\User as ModelAlias;\nuse App\\Services\\User as ModelAlias;\n";
        let diagnostics = detect_duplicate_imports(source);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn detects_missing_return_type_for_function() {
        let source = "<?php\nfunction loadUser($id) { return $id; }\n";
        let diagnostics = detect_missing_return_types(source);
        assert!(diagnostics
            .iter()
            .any(|d| d.message == "Missing return type: loadUser()"));
    }

    #[test]
    fn skips_missing_return_type_when_phpdoc_return_exists() {
        let source = "<?php\n/** @return User */\nfunction loadUser($id) { return $id; }\n";
        let diagnostics = detect_missing_return_types(source);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn skips_missing_return_type_for_magic_methods() {
        let source = "<?php\nclass A { function __construct($x) { } }\n";
        let diagnostics = detect_missing_return_types(source);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn detects_missing_return_type_for_class_method() {
        let source = "<?php\nclass UserService {\n  function getData() { return []; }\n}\n";
        let diagnostics = detect_missing_return_types(source);
        assert!(diagnostics
            .iter()
            .any(|d| d.message == "Missing return type: getData()"));
    }

    #[test]
    fn detects_missing_return_type_for_static_method() {
        let source =
            "<?php\nclass A {\n  static function factory() { return new self(); }\n}\n";
        let diagnostics = detect_missing_return_types(source);
        assert!(diagnostics
            .iter()
            .any(|d| d.message == "Missing return type: factory()"));
    }

    #[test]
    fn detects_missing_return_type_for_abstract_method() {
        let source = "<?php\nabstract class A {\n  abstract function run();\n}\n";
        let diagnostics = detect_missing_return_types(source);
        assert!(diagnostics
            .iter()
            .any(|d| d.message == "Missing return type: run()"));
    }

    #[test]
    fn detects_missing_return_type_for_interface_method() {
        let source = "<?php\ninterface Repo {\n  public function find();\n}\n";
        let diagnostics = detect_missing_return_types(source);
        assert!(diagnostics
            .iter()
            .any(|d| d.message == "Missing return type: find()"));
    }

    #[test]
    fn detects_missing_return_type_for_trait_method() {
        let source = "<?php\ntrait Loggable {\n  public function log($msg) { echo $msg; }\n}\n";
        let diagnostics = detect_missing_return_types(source);
        assert!(diagnostics
            .iter()
            .any(|d| d.message == "Missing return type: log()"));
    }

    #[test]
    fn detects_missing_return_type_for_magic_call() {
        let source =
            "<?php\nclass DynamicProxy {\n  public function __call($name, $args) { return null; }\n}\n";
        let diagnostics = detect_missing_return_types(source);
        assert!(diagnostics
            .iter()
            .any(|d| d.message == "Missing return type: __call()"));
    }

    #[test]
    fn skips_reference_usage_for_unused_check() {
        let source =
            "<?php\nfunction run() {\n  $value = 1;\n  $ref = &$value;\n  return $ref;\n}\n";
        let diagnostics = detect_unused_variables(source);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn detects_unused_variables_in_nested_functions() {
        let source = "<?php\nfunction outer() {\n  $outerUnused = 1;\n  function inner() {\n    $innerUnused = 2;\n    return 1;\n  }\n  return 0;\n}\n";
        let diagnostics = detect_unused_variables(source);
        assert!(diagnostics
            .iter()
            .any(|d| d.message == "Unused variable: $outerUnused"));
        assert!(diagnostics
            .iter()
            .any(|d| d.message == "Unused variable: $innerUnused"));
    }

    #[test]
    fn detects_unclosed_opening_brace() {
        let source = "<?php\nif ($x) {\n  echo $x;\n";
        let diagnostics = detect_brace_mismatch(source);
        assert!(diagnostics
            .iter()
            .any(|d| d.message == "Unclosed opening brace '{'"));
    }

    #[test]
    fn detects_unexpected_closing_brace() {
        let source = "<?php\n}\n";
        let diagnostics = detect_brace_mismatch(source);
        assert!(diagnostics
            .iter()
            .any(|d| d.message == "Unexpected closing brace '}'"));
    }

    #[test]
    fn ignores_braces_inside_strings_for_brace_diagnostics() {
        let source = "<?php\n$text = '{';\n";
        let diagnostics = detect_brace_mismatch(source);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn detects_assignment_in_if_condition() {
        let source = "<?php\nif ($x = 1) { echo $x; }\n";
        let diagnostics = detect_operator_confusion(source);
        assert!(diagnostics
            .iter()
            .any(|d| d.message == "Suspicious assignment '=' in conditional expression"));
    }

    #[test]
    fn skips_comparison_operator_in_if_condition() {
        let source = "<?php\nif ($x == 1) { echo $x; }\nif ($x === 1) { echo $x; }\n";
        let diagnostics = detect_operator_confusion(source);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn skips_assignment_outside_condition() {
        let source = "<?php\n$x = 1;\n";
        let diagnostics = detect_operator_confusion(source);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn detects_undefined_function_calls() {
        let source = "<?php\n$result = App\\NotDefined\\run(1);\n";
        let diagnostics = detect_undefined_function_calls(source);
        assert!(diagnostics
            .iter()
            .any(|d| d.message == "Undefined function: App\\NotDefined\\run()"));
    }

    #[test]
    fn detects_leading_backslash_namespaced_function_calls() {
        let source = "<?php\n$result = \\App\\NotDefined\\run(1);\n";
        let diagnostics = detect_undefined_function_calls(source);
        assert!(diagnostics
            .iter()
            .any(|d| d.message == "Undefined function: \\App\\NotDefined\\run()"));
    }

    #[test]
    fn skips_defined_namespaced_function_calls() {
        let source = "<?php\nnamespace App\\Core;\nfunction run_test() {}\nApp\\Core\\run_test();\n";
        let diagnostics = detect_undefined_function_calls(source);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn skips_defined_namespaced_function_calls_with_leading_backslash() {
        let source =
            "<?php\nnamespace App\\Core;\nfunction run_test() {}\n\\App\\Core\\run_test();\n";
        let diagnostics = detect_undefined_function_calls(source);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn skips_global_builtin_with_leading_backslash_for_safety() {
        let source = "<?php\n\\strlen('x');\n";
        let diagnostics = detect_undefined_function_calls(source);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn skips_unqualified_function_calls_for_safety() {
        let source = "<?php\nrun_test();\nstrlen('x');\n";
        let diagnostics = detect_undefined_function_calls(source);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn skips_method_and_static_calls_for_undefined_function_check() {
        let source = "<?php\n$service->run();\nService::make();\n";
        let diagnostics = detect_undefined_function_calls(source);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn skips_known_namespaced_function_calls_from_metadata() {
        let source = "<?php\n$result = App\\Meta\\resolve(1);\n";
        let known = HashSet::from(["app\\meta\\resolve".to_string()]);
        let diagnostics = detect_undefined_function_calls_with_known(source, &known);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn detects_unknown_namespaced_function_calls_when_known_set_is_empty() {
        let source = "<?php\n$result = App\\Meta\\resolve(1);\n";
        let known = HashSet::new();
        let diagnostics = detect_undefined_function_calls_with_known(source, &known);
        assert!(diagnostics
            .iter()
            .any(|d| d.message == "Undefined function: App\\Meta\\resolve()"));
    }

    #[test]
    fn skips_compound_assignments_in_condition() {
        let source = "<?php\nif ($x ??= []) { }\nif ($s .= 'ok') { }\nif ($n **= 2) { }\n";
        let diagnostics = detect_operator_confusion(source);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn detects_assignment_in_while_and_elseif_conditions() {
        let source = "<?php\nwhile ($x = next()) { }\nelseif ($y = 1) { }\n";
        let diagnostics = detect_operator_confusion(source);
        assert_eq!(
            diagnostics
                .iter()
                .filter(|d| d.message == "Suspicious assignment '=' in conditional expression")
                .count(),
            2
        );
    }

    #[test]
    fn ignores_builtin_variables_in_undefined_check() {
        let source = "<?php\n$value = $_POST['k'] ?? $_GET['q'] ?? null;\n";
        let diagnostics = detect_undefined_variables(source);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn treats_function_parameters_as_declared_variables() {
        let source = "<?php\nfunction run(string $name, int $age) {\n  $value = $name;\n  return $age + $missing;\n}\n";
        let diagnostics = detect_undefined_variables(source);

        assert!(diagnostics.iter().any(|d| d.message == "Undefined variable: $missing"));
        assert!(!diagnostics.iter().any(|d| d.message == "Undefined variable: $name"));
        assert!(!diagnostics.iter().any(|d| d.message == "Undefined variable: $age"));
    }

    #[test]
    fn treats_foreach_variables_as_declared() {
        let source = "<?php\nforeach ($items as $item) {\n  $sum = $item;\n}\n";
        let diagnostics = detect_undefined_variables(source);

        assert!(diagnostics.iter().any(|d| d.message == "Undefined variable: $items"));
        assert!(!diagnostics.iter().any(|d| d.message == "Undefined variable: $item"));
    }

    #[test]
    fn isolates_function_scopes_for_variables() {
        let source = "<?php\nfunction first($x) { $y = $x; }\nfunction second() { return $x + $y; }\n";
        let diagnostics = detect_undefined_variables(source);

        assert!(diagnostics.iter().any(|d| d.message == "Undefined variable: $x"));
        assert!(diagnostics.iter().any(|d| d.message == "Undefined variable: $y"));
    }

    #[test]
    fn handles_global_static_and_catch_declarations() {
        let source = "<?php\n$shared = 1;\nfunction run() {\n  global $shared;\n  static $counter = 0;\n  try { throw new Exception(); } catch (Exception $e) { $counter = $counter + 1; }\n  return $shared + $counter + strlen($e->getMessage());\n}\n";
        let diagnostics = detect_undefined_variables(source);

        assert!(!diagnostics.iter().any(|d| d.message == "Undefined variable: $shared"));
        assert!(!diagnostics.iter().any(|d| d.message == "Undefined variable: $counter"));
        assert!(!diagnostics.iter().any(|d| d.message == "Undefined variable: $e"));
    }

    #[test]
    fn handles_closure_use_variables() {
        let source = "<?php\n$outer = 10;\n$fn = function () use ($outer) { return $outer + 1; };\n";
        let diagnostics = detect_undefined_variables(source);
        assert!(!diagnostics.iter().any(|d| d.message == "Undefined variable: $outer"));
    }

    #[test]
    fn treats_phpdoc_var_annotations_as_declared_variables() {
        let source = "<?php\n/** @var User $user */\n$user->id();\n";
        let diagnostics = detect_undefined_variables(source);
        assert!(!diagnostics.iter().any(|d| d.message == "Undefined variable: $user"));
    }

    #[test]
    fn treats_line_comment_var_annotations_as_declared_variables() {
        let source = "<?php\n// @var Collection<int,User> $users\n$users->first();\n";
        let diagnostics = detect_undefined_variables(source);
        assert!(!diagnostics.iter().any(|d| d.message == "Undefined variable: $users"));
    }

    #[test]
    fn treats_inline_var_annotations_as_declared_variables() {
        let source = "<?php\n/** @var Product $product */ $value = $product->getId();\n";
        let diagnostics = detect_undefined_variables(source);
        assert!(!diagnostics.iter().any(|d| d.message == "Undefined variable: $product"));
    }

    #[test]
    fn ignores_var_annotations_inside_strings() {
        let source = "<?php\n$text = \"@var User $ghost\";\n$real = 1;\n$use = $real + $ghost;\n";
        let diagnostics = detect_undefined_variables(source);
        assert!(diagnostics.iter().any(|d| d.message == "Undefined variable: $ghost"));
    }

    #[test]
    fn treats_psalm_var_annotations_as_declared_variables() {
        let source = "<?php\n/** @psalm-var User $user */\n$user->id();\n";
        let diagnostics = detect_undefined_variables(source);
        assert!(!diagnostics.iter().any(|d| d.message == "Undefined variable: $user"));
    }

    #[test]
    fn treats_phpstan_var_annotations_as_declared_variables() {
        let source = "<?php\n/* @phpstan-var Collection<int,User> $users */\n$users->first();\n";
        let diagnostics = detect_undefined_variables(source);
        assert!(!diagnostics.iter().any(|d| d.message == "Undefined variable: $users"));
    }

    #[test]
    fn builds_reference_locations_for_symbol() {
        let uri_main = Url::parse("file:///tmp/main.php").expect("main uri");
        let uri_other = Url::parse("file:///tmp/other.php").expect("other uri");
        let symbol = PhpSymbol {
            name: "run_test".to_string(),
            namespace: None,
            kind: SymbolKind::FUNCTION,
            parameters: Vec::new(),
            return_type: None,
            range: Range::new(Position::new(1, 9), Position::new(1, 10)),
        };

        let docs = HashMap::from([
            (
                uri_main.clone(),
                "<?php\nfunction run_test() {}\nrun_test();\n".to_string(),
            ),
            (uri_other.clone(), "<?php\nrun_test();\n".to_string()),
        ]);

        let refs = reference_locations_for_symbol(&symbol, &uri_main, &docs);
        assert_eq!(refs.len(), 2);
        assert!(refs.iter().any(|loc| loc.uri == uri_main));
        assert!(refs.iter().any(|loc| loc.uri == uri_other));
    }

    #[test]
    fn formats_reference_count_titles() {
        assert_eq!(reference_count_title(0), "No references");
        assert_eq!(reference_count_title(1), "1 reference");
        assert_eq!(reference_count_title(2), "2 references");
    }

    #[test]
    fn collects_block_folding_ranges() {
        let source = "<?php\nclass A {\n  function run() {\n    return 1;\n  }\n}\n";
        let ranges = collect_folding_ranges(source);
        assert!(ranges.iter().any(|r| r.start_line == 1 && r.end_line == 5));
        assert!(ranges.iter().any(|r| r.start_line == 2 && r.end_line == 4));
    }

    #[test]
    fn ignores_braces_in_comments_and_strings_for_folding() {
        let source = "<?php\n// {\n$text = \"}\";\nif (true) {\n  echo $text;\n}\n";
        let ranges = collect_folding_ranges(source);
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].start_line, 3);
        assert_eq!(ranges[0].end_line, 5);
    }

    #[test]
    fn collects_import_block_folding_range() {
        let source = "<?php\nuse App\\A;\nuse App\\B;\nclass C {}\n";
        let ranges = collect_folding_ranges(source);
        assert!(ranges.iter().any(|r| {
            r.start_line == 1
                && r.end_line == 2
                && r.kind == Some(tower_lsp::lsp_types::FoldingRangeKind::Imports)
        }));
    }

    #[test]
    fn parses_class_relationship_targets() {
        let (extends_target, implements_targets) =
            extract_class_relationship_targets("class UserRepo extends BaseRepo implements UserInterface, Loggable {");
        assert_eq!(extends_target.as_deref(), Some("BaseRepo"));
        assert_eq!(implements_targets.len(), 2);
        assert!(implements_targets.contains(&"UserInterface".to_string()));
        assert!(implements_targets.contains(&"Loggable".to_string()));
    }

    #[test]
    fn collects_class_implementations_for_target() {
        let iface_uri = Url::parse("file:///tmp/contracts.php").expect("iface uri");
        let impl_uri = Url::parse("file:///tmp/repos.php").expect("impl uri");

        let docs = HashMap::from([
            (
                iface_uri,
                "<?php\nnamespace App\\Contracts;\ninterface UserRepository {}\n".to_string(),
            ),
            (
                impl_uri.clone(),
                "<?php\nnamespace App\\Infra;\nuse App\\Contracts\\UserRepository;\nclass DbUserRepository implements UserRepository {}\n".to_string(),
            ),
        ]);

        let symbols = HashMap::from([(
            impl_uri.clone(),
            vec![PhpSymbol {
                name: "DbUserRepository".to_string(),
                namespace: Some("App\\Infra".to_string()),
                kind: SymbolKind::CLASS,
                parameters: Vec::new(),
                return_type: None,
                range: Range::new(Position::new(3, 6), Position::new(3, 22)),
            }],
        )]);

        let locations = collect_class_implementation_locations(
            "App\\Contracts\\UserRepository",
            &docs,
            &symbols,
        );
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].uri, impl_uri);
    }

    #[test]
    fn collects_class_implementation_for_second_interface_in_list() {
        let impl_uri = Url::parse("file:///tmp/repos2.php").expect("impl uri");
        let docs = HashMap::from([(
            impl_uri.clone(),
            "<?php\nnamespace App\\Infra;\nclass Repo implements FirstContract, SecondContract {}\n".to_string(),
        )]);

        let symbols = HashMap::from([(
            impl_uri.clone(),
            vec![PhpSymbol {
                name: "Repo".to_string(),
                namespace: Some("App\\Infra".to_string()),
                kind: SymbolKind::CLASS,
                parameters: Vec::new(),
                return_type: None,
                range: Range::new(Position::new(2, 6), Position::new(2, 10)),
            }],
        )]);

        let locations = collect_class_implementation_locations(
            "SecondContract",
            &docs,
            &symbols,
        );
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].uri, impl_uri);
    }

    #[test]
    fn collects_class_implementation_with_alias_and_line_tolerance() {
        let impl_uri = Url::parse("file:///tmp/repos3.php").expect("impl uri");
        let docs = HashMap::from([(
            impl_uri.clone(),
            "<?php\nnamespace App\\Infra;\nuse App\\Contracts\\UserRepository as RepoContract;\nclass DbUserRepository implements RepoContract {}\n".to_string(),
        )]);

        let symbols = HashMap::from([(
            impl_uri.clone(),
            vec![PhpSymbol {
                name: "DbUserRepository".to_string(),
                namespace: Some("App\\Infra".to_string()),
                kind: SymbolKind::CLASS,
                parameters: Vec::new(),
                return_type: None,
                // intentionally offset by one line to validate tolerant matching
                range: Range::new(Position::new(2, 6), Position::new(2, 22)),
            }],
        )]);

        let locations = collect_class_implementation_locations(
            "App\\Contracts\\UserRepository",
            &docs,
            &symbols,
        );
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].uri, impl_uri);
    }

    #[test]
    fn builds_function_parameter_map_from_symbols() {
        let uri = Url::parse("file:///tmp/a.php").expect("uri");
        let symbols = HashMap::from([(
            uri,
            vec![PhpSymbol {
                name: "run_test".to_string(),
                namespace: None,
                kind: SymbolKind::FUNCTION,
                parameters: vec!["string $name".to_string(), "int $age".to_string()],
                return_type: Some("void".to_string()),
                range: Range::new(Position::new(1, 9), Position::new(1, 10)),
            }],
        )]);

        let map = build_function_parameter_map(&symbols);
        assert!(map.contains_key("run_test"));
        assert_eq!(map["run_test"].len(), 2);
    }

    #[test]
    fn collects_parameter_inlay_hints_for_known_function_calls() {
        let source = "<?php\nfunction run_test(string $name, int $age) {}\nrun_test('a', 1);\n";
        let map = HashMap::from([(
            "run_test".to_string(),
            vec!["string $name".to_string(), "int $age".to_string()],
        )]);

        let hints = collect_parameter_inlay_hints_for_range(source, 2, 2, &map);
        assert!(!hints.is_empty());
        assert!(hints.iter().all(|hint| hint.2.ends_with(':')));
    }

    #[test]
    fn collects_parameter_inlay_hints_for_by_reference_arguments() {
        let source = "<?php\nfunction hydrate(array &$items, ?App\\Models\\User $owner) {}\nhydrate($items, $owner);\n";
        let map = HashMap::from([(
            "hydrate".to_string(),
            vec![
                "array &$items".to_string(),
                "?App\\Models\\User $owner".to_string(),
            ],
        )]);

        let hints = collect_parameter_inlay_hints_for_range(source, 2, 2, &map);
        assert!(hints.iter().any(|hint| hint.2 == "&items (array):"));
        assert!(
            hints
                .iter()
                .any(|hint| hint.2 == "owner (?App\\Models\\User):")
        );
    }

    #[test]
    fn collects_return_type_inlay_hints_for_functions_in_range() {
        let uri = Url::parse("file:///tmp/a.php").expect("uri");
        let symbols = HashMap::from([(
            uri.clone(),
            vec![
                PhpSymbol {
                    name: "run_test".to_string(),
                    namespace: None,
                    kind: SymbolKind::FUNCTION,
                    parameters: vec!["string $name".to_string()],
                    return_type: Some("int".to_string()),
                    range: Range::new(Position::new(2, 9), Position::new(2, 17)),
                },
                PhpSymbol {
                    name: "no_hint".to_string(),
                    namespace: None,
                    kind: SymbolKind::FUNCTION,
                    parameters: Vec::new(),
                    return_type: None,
                    range: Range::new(Position::new(5, 9), Position::new(5, 16)),
                },
            ],
        )]);

        let hints = collect_return_type_inlay_hints_for_range(&symbols, &uri, 1, 3);
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].position, Position::new(2, 17));
        assert!(matches!(
            &hints[0].label,
            InlayHintLabel::String(label) if label == ": int"
        ));
    }

    #[test]
    fn skips_return_type_inlay_hints_outside_range() {
        let uri = Url::parse("file:///tmp/a.php").expect("uri");
        let symbols = HashMap::from([(
            uri.clone(),
            vec![PhpSymbol {
                name: "run_test".to_string(),
                namespace: None,
                kind: SymbolKind::FUNCTION,
                parameters: vec!["string $name".to_string()],
                return_type: Some("int".to_string()),
                range: Range::new(Position::new(6, 9), Position::new(6, 17)),
            }],
        )]);

        let hints = collect_return_type_inlay_hints_for_range(&symbols, &uri, 1, 3);
        assert!(hints.is_empty());
    }

    #[test]
    fn skips_parameter_hints_for_function_declaration_line() {
        let source = "<?php\nfunction run_test(string $name, int $age) {}\n";
        let map = HashMap::from([(
            "run_test".to_string(),
            vec!["string $name".to_string(), "int $age".to_string()],
        )]);

        let hints = collect_parameter_inlay_hints_for_range(source, 1, 1, &map);
        assert!(hints.is_empty());
    }

    #[test]
    fn collects_parameter_hints_with_array_and_object_args() {
        let source = "<?php\nrun_test(['a', 'b'], ['k' => ['x' => 1]], $value);\n";
        let map = HashMap::from([(
            "run_test".to_string(),
            vec!["array $first".to_string(), "array $second".to_string(), "mixed $third".to_string()],
        )]);

        let hints = collect_parameter_inlay_hints_for_range(source, 1, 1, &map);
        assert_eq!(hints.len(), 3);
    }

    #[test]
    fn treats_literal_argument_as_non_empty_content() {
        let chars = "run_test('x')".chars().collect::<Vec<_>>();
        let mask = vec![true; chars.len()];
        assert!(arg_has_content(&chars, &mask, 9, 12));
    }

    #[test]
    fn formats_function_symbol_for_hover() {
        let symbol = PhpSymbol {
            name: "run_test".to_string(),
            namespace: Some("App\\Utils".to_string()),
            kind: SymbolKind::FUNCTION,
            parameters: vec!["string $name".to_string(), "int $age".to_string()],
            return_type: Some("string".to_string()),
            range: Range::new(Position::new(2, 0), Position::new(2, 8)),
        };

        let rendered = format_symbol_for_hover(&symbol);
        assert!(rendered.contains("**Function** `run_test`"));
        assert!(rendered.contains("**Namespace:** `App\\Utils`"));
        assert!(rendered.contains("**Signature:** `run_test(string $name, int $age): string`"));
    }

    #[test]
    fn extracts_template_params_from_docblock_tags() {
        let docblock = "\
/**
 * @template TKey of array-key
 * @psalm-template TValue as object
 * @phpstan-template-covariant TModel
 */";

        let params = extract_template_params_from_docblock(docblock);
        assert_eq!(params, vec!["TKey", "TValue", "TModel"]);
    }

    #[test]
    fn extracts_mixin_types_from_docblock_tags() {
        let docblock = "\
/**
 * @mixin App\\Support\\MagicMixin
 * @phpstan-mixin ?App\\Services\\ExtraService|null
 */";

        let mixins = extract_mixin_types_from_docblock(docblock);
        assert_eq!(
            mixins,
            vec![
                "App\\Support\\MagicMixin".to_string(),
                "App\\Services\\ExtraService".to_string(),
            ]
        );
    }

    #[test]
    fn formats_function_symbol_for_hover_with_templates() {
        let symbol = PhpSymbol {
            name: "map_items".to_string(),
            namespace: Some("App\\Utils".to_string()),
            kind: SymbolKind::FUNCTION,
            parameters: vec!["array $items".to_string()],
            return_type: Some("array".to_string()),
            range: Range::new(Position::new(2, 0), Position::new(2, 9)),
        };

        let rendered = format_symbol_for_hover_with_templates(&symbol, &["TItem".to_string()]);
        assert!(rendered.contains("**Templates:** `TItem`"));
        assert!(rendered.contains("**Signature:** `map_items<TItem>(array $items): array`"));
    }

    #[test]
    fn builds_symbol_display_name_with_templates() {
        let plain = symbol_display_name_with_templates("run", &[]);
        assert_eq!(plain, "run");

        let templated = symbol_display_name_with_templates(
            "map_items",
            &["TKey".to_string(), "TValue".to_string()],
        );
        assert_eq!(templated, "map_items<TKey, TValue>");
    }

    #[test]
    fn formats_class_symbol_for_hover() {
        let symbol = PhpSymbol {
            name: "UserService".to_string(),
            namespace: Some("App\\Services".to_string()),
            kind: SymbolKind::CLASS,
            parameters: Vec::new(),
            return_type: None,
            range: Range::new(Position::new(4, 6), Position::new(4, 17)),
        };

        let rendered = format_symbol_for_hover(&symbol);
        assert!(rendered.contains("**Class** `UserService`"));
        assert!(rendered.contains("**Namespace:** `App\\Services`"));
        assert!(!rendered.contains("**Signature:**"));
    }

    #[test]
    fn extracts_symbol_range_for_full_identifier() {
        let source = "<?php\nfunction run_test() {}\nconst APP_VERSION = '1';\n";
        let symbols = extract_symbols(source);

        let function = symbols
            .iter()
            .find(|symbol| symbol.name == "run_test")
            .expect("function symbol");
        assert_eq!(function.range.start.character, 9);
        assert_eq!(function.range.end.character, 17);

        let constant = symbols
            .iter()
            .find(|symbol| symbol.name == "APP_VERSION")
            .expect("constant symbol");
        assert_eq!(constant.range.start.character, 6);
        assert_eq!(constant.range.end.character, 17);
    }

    #[test]
    fn prefers_current_document_symbol_for_hover_queries() {
        let current_uri = Url::parse("file:///tmp/current.php").expect("current uri");
        let external_uri = Url::parse("file:///tmp/external.php").expect("external uri");
        let current_symbol = PhpSymbol {
            name: "User".to_string(),
            namespace: Some("App\\Local".to_string()),
            kind: SymbolKind::CLASS,
            parameters: Vec::new(),
            return_type: None,
            range: Range::new(Position::new(2, 6), Position::new(2, 10)),
        };
        let external_symbol = PhpSymbol {
            name: "User".to_string(),
            namespace: Some("App\\External".to_string()),
            kind: SymbolKind::CLASS,
            parameters: Vec::new(),
            return_type: None,
            range: Range::new(Position::new(5, 6), Position::new(5, 10)),
        };

        let index = HashMap::from([
            (current_uri.clone(), vec![current_symbol]),
            (external_uri, vec![external_symbol]),
        ]);
        let queries = vec!["User".to_string()];

        let (_, matched) = find_symbol_location_for_queries(&current_uri, &index, &queries)
            .expect("matched symbol");
        assert_eq!(matched.namespace.as_deref(), Some("App\\Local"));
    }

    #[test]
    fn builds_php_manual_url_for_builtin_function_name() {
        let url = php_manual_function_url("str_replace").expect("manual url");
        assert_eq!(url, "https://www.php.net/manual/en/function.str-replace.php");
    }

    #[test]
    fn skips_php_manual_url_for_variable_or_namespaced_symbol() {
        assert!(php_manual_function_url("$value").is_none());
        assert!(php_manual_function_url("App\\Service\\run").is_none());
    }

    #[test]
    fn builds_standard_import_action_when_short_name_is_available() {
        let imports = HashMap::new();
        let action = import_action_for_fqn("Vendor\\Package\\Client", &imports).expect("import action");

        assert_eq!(action.0, "use Vendor\\Package\\Client;\n");
        assert_eq!(action.1, "Add use Vendor\\Package\\Client");
    }

    #[test]
    fn builds_aliased_import_action_when_short_name_conflicts() {
        let imports = HashMap::from([(
            "Client".to_string(),
            "App\\Http\\Client".to_string(),
        )]);

        let action = import_action_for_fqn("Vendor\\Package\\Client", &imports).expect("aliased action");
        assert_eq!(action.0, "use Vendor\\Package\\Client as ClientAlias;\n");
        assert_eq!(action.1, "Add use Vendor\\Package\\Client as ClientAlias");
    }

    #[test]
    fn skips_import_action_for_existing_fqn() {
        let imports = HashMap::from([(
            "Client".to_string(),
            "Vendor\\Package\\Client".to_string(),
        )]);

        let action = import_action_for_fqn("Vendor\\Package\\Client", &imports);
        assert!(action.is_none());
    }

    #[test]
    fn increments_alias_suffix_until_available() {
        let imports = HashMap::from([
            ("Client".to_string(), "App\\Http\\Client".to_string()),
            (
                "ClientAlias".to_string(),
                "App\\Contracts\\Client".to_string(),
            ),
            (
                "ClientAlias2".to_string(),
                "App\\Legacy\\Client".to_string(),
            ),
        ]);

        let action = import_action_for_fqn("Vendor\\Package\\Client", &imports).expect("aliased action");
        assert_eq!(action.0, "use Vendor\\Package\\Client as ClientAlias3;\n");
        assert_eq!(action.1, "Add use Vendor\\Package\\Client as ClientAlias3");
    }

    #[test]
    fn detects_single_document_link_url() {
        let text = "See https://example.com/docs for details.";
        let links = detect_http_urls(text);

        assert_eq!(links.len(), 1);
        assert_eq!(links[0].range.start.character, 4);
        assert_eq!(links[0].range.end.character, 28);
        assert_eq!(
            links[0].target.as_ref().map(|url| url.as_str()),
            Some("https://example.com/docs")
        );
    }

    #[test]
    fn detects_multiple_document_link_urls_across_lines() {
        let text = "A http://example.com\nB https://docs.example.com/x";
        let links = detect_http_urls(text);

        assert_eq!(links.len(), 2);
        assert_eq!(links[0].range.start.line, 0);
        assert_eq!(links[1].range.start.line, 1);
        assert_eq!(
            links[1].target.as_ref().map(|url| url.as_str()),
            Some("https://docs.example.com/x")
        );
    }

    #[test]
    fn trims_trailing_punctuation_from_document_links() {
        let text = "See https://example.com/docs; then continue.";
        let links = detect_http_urls(text);

        assert_eq!(links.len(), 1);
        assert_eq!(
            links[0].target.as_ref().map(|url| url.as_str()),
            Some("https://example.com/docs")
        );
    }

    #[test]
    fn tokenizes_variable_and_definition_symbols() {
        let source = "<?php\nnamespace App\\Models;\nclass User {}\nfunction run_test() {\n  $name = 'a';\n  return $name;\n}\n";
        let tokens = tokenize_php_document(source);

        assert!(tokens.iter().any(|token| token.token_type == 5));
        assert!(tokens.iter().any(|token| token.token_type == 2));
        assert!(tokens.iter().any(|token| token.token_type == 1));
        assert!(tokens.iter().filter(|token| token.token_type == 0).count() >= 2);
    }

    #[test]
    fn compresses_semantic_tokens_to_lsp_delta_format() {
        let tokens = vec![
            ParsedSemanticToken {
                line: 0,
                char: 0,
                length: 3,
                token_type: 2,
            },
            ParsedSemanticToken {
                line: 0,
                char: 5,
                length: 4,
                token_type: 1,
            },
            ParsedSemanticToken {
                line: 2,
                char: 2,
                length: 5,
                token_type: 0,
            },
        ];

        let data = compress_tokens_to_semantic_data(tokens);
        assert_eq!(data.len(), 3);
        assert_eq!(data[0].delta_line, 0);
        assert_eq!(data[0].delta_start, 0);
        assert_eq!(data[0].length, 3);
        assert_eq!(data[0].token_type, 2);
        assert_eq!(data[1].delta_line, 0);
        assert_eq!(data[1].delta_start, 5);
        assert_eq!(data[2].delta_line, 2);
        assert_eq!(data[2].delta_start, 2);
    }

    #[test]
    fn semantic_tokenizer_ignores_variables_in_comments_and_strings() {
        let source = "<?php\n$real = 1;\n// $commented\n$text = \"$inside_string\";\n$real = $real + 1;\n";
        let tokens = tokenize_php_document(source);

        assert!(tokens.iter().any(|token| token.token_type == 0));
        assert!(!tokens.iter().any(|token| token.line == 2 && token.token_type == 0));
        let line_three_variable_tokens = tokens
            .iter()
            .filter(|token| token.line == 3 && token.token_type == 0)
            .collect::<Vec<_>>();
        assert_eq!(line_three_variable_tokens.len(), 1);
        assert_eq!(line_three_variable_tokens[0].char, 0);
    }

    #[test]
    fn semantic_token_delta_is_relative_within_same_line() {
        let source = "<?php\n$first = $second;\n";
        let mut tokens = tokenize_php_document(source)
            .into_iter()
            .filter(|token| token.token_type == 0)
            .collect::<Vec<_>>();
        tokens.sort_by(|a, b| a.char.cmp(&b.char));

        let data = compress_tokens_to_semantic_data(tokens);
        assert_eq!(data.len(), 2);
        assert_eq!(data[0].delta_line, 1);
        assert_eq!(data[0].delta_start, 0);
        assert_eq!(data[1].delta_line, 0);
        assert!(data[1].delta_start > 0);
    }

    #[test]
    fn semantic_tokenizer_ignores_declarations_in_comments_and_strings() {
        let source = "<?php\n// namespace Fake\\Ns; class FakeClass {} function fake_fn() {}\n$text = \"namespace Fake\\Ns; class FakeClass {}\";\nnamespace Real\\Ns;\nclass RealClass {}\nfunction real_fn() {}\n";
        let tokens = tokenize_php_document(source);

        let namespaces = tokens.iter().filter(|token| token.token_type == 5).count();
        let classes = tokens.iter().filter(|token| token.token_type == 2).count();
        let functions = tokens.iter().filter(|token| token.token_type == 1).count();

        assert_eq!(namespaces, 1);
        assert_eq!(classes, 1);
        assert_eq!(functions, 1);
    }

    #[test]
    fn format_document_removes_trailing_whitespace() {
        let input = "<?php\necho 'test';   \necho 'done';  ";
        let output = format_document(input);

        assert!(!output.contains(";   \n"));
        assert!(!output.contains(";  \n"));
        assert_eq!(output, "<?php\necho 'test';\necho 'done';\n");
    }

    #[test]
    fn format_document_limits_consecutive_blank_lines_to_two() {
        let input = "<?php\necho 'a';\n\n\n\n\necho 'b';\n";
        let output = format_document(input);

        assert!(output.contains("echo 'a';\n\n\n"));
        assert!(!output.contains("\n\n\n\n"));
    }

    #[test]
    fn format_document_ensures_single_trailing_newline() {
        let input = "<?php\necho 'ok';\n\n\n";
        let output = format_document(input);

        assert!(output.ends_with('\n'));
        assert!(!output.ends_with("\n\n"));
    }

    #[test]
    fn format_document_keeps_already_formatted_text() {
        let input = "<?php\necho 'ok';\n";
        let output = format_document(input);
        assert_eq!(output, input);
    }

    #[test]
    fn document_end_position_handles_newline_edge_cases() {
        assert_eq!(document_end_position(""), Position::new(0, 0));
        assert_eq!(document_end_position("<?php"), Position::new(0, 5));
        assert_eq!(document_end_position("<?php\n"), Position::new(1, 0));
        assert_eq!(document_end_position("<?php\necho 1;"), Position::new(1, 7));
    }

    #[test]
    fn format_document_handles_empty_input() {
        assert_eq!(format_document(""), "");
        assert_eq!(format_document("\n\n"), "");
        assert_eq!(format_document("   \n\t\n"), "");
    }

    #[test]
    fn format_document_handles_single_line_without_newline() {
        let input = "<?php echo 'x';";
        let output = format_document(input);
        assert_eq!(output, "<?php echo 'x';\n");
    }

    #[test]
    fn format_document_handles_no_trailing_newline_multiline() {
        let input = "<?php\n\n\n\necho 'x';";
        let output = format_document(input);
        assert_eq!(output, "<?php\n\n\necho 'x';\n");
    }

    #[test]
    fn builds_var_dump_delete_action_for_debug_diagnostic() {
        let text = "<?php\nvar_dump($x);\n$y = 1;\n";
        let uri = Url::parse("file:///tmp/test.php").expect("uri");
        let diagnostic = Diagnostic {
            range: Range::new(Position::new(1, 0), Position::new(1, 8)),
            severity: Some(DiagnosticSeverity::HINT),
            message: "Avoid leaving debug output in committed code.".to_string(),
            source: Some("vscode-ls-php".to_string()),
            ..Diagnostic::default()
        };

        let action = var_dump_delete_action(&diagnostic, text, &uri).expect("action");
        let CodeActionOrCommand::CodeAction(code_action) = action else {
            panic!("expected code action");
        };
        assert_eq!(code_action.title, "Remove debug var_dump");

        let edit = code_action.edit.expect("edit");
        let changes = edit.changes.expect("changes");
        let text_edits = changes.get(&uri).expect("uri edits");
        assert_eq!(text_edits.len(), 1);
        assert_eq!(text_edits[0].range.start, Position::new(1, 0));
        assert_eq!(text_edits[0].range.end, Position::new(2, 0));
        assert_eq!(text_edits[0].new_text, "");
    }

    #[test]
    fn skips_var_dump_delete_action_for_unrelated_diagnostic() {
        let text = "<?php\nvar_dump($x);\n";
        let uri = Url::parse("file:///tmp/test.php").expect("uri");
        let diagnostic = Diagnostic {
            range: Range::new(Position::new(1, 0), Position::new(1, 8)),
            severity: Some(DiagnosticSeverity::HINT),
            message: "Other diagnostic".to_string(),
            source: Some("vscode-ls-php".to_string()),
            ..Diagnostic::default()
        };

        let action = var_dump_delete_action(&diagnostic, text, &uri);
        assert!(action.is_none());
    }

    #[test]
    fn builds_unused_import_remove_action() {
        let text = "<?php\nuse App\\Models\\User;\nclass A {}\n";
        let uri = Url::parse("file:///tmp/a.php").expect("uri");
        let diagnostic = Diagnostic {
            range: Range::new(Position::new(1, 15), Position::new(1, 19)),
            severity: Some(DiagnosticSeverity::HINT),
            message: "Unused import: User".to_string(),
            source: Some("vscode-ls-php".to_string()),
            ..Diagnostic::default()
        };

        let action = unused_import_remove_action(&diagnostic, text, &uri).expect("action");
        let CodeActionOrCommand::CodeAction(code_action) = action else {
            panic!("expected code action");
        };
        assert_eq!(code_action.title, "Remove unused import User");

        let edit = code_action.edit.expect("edit");
        let changes = edit.changes.expect("changes");
        let text_edits = changes.get(&uri).expect("uri edits");
        assert_eq!(text_edits.len(), 1);
        assert_eq!(text_edits[0].range.start, Position::new(1, 0));
        assert_eq!(text_edits[0].range.end, Position::new(2, 0));
        assert_eq!(text_edits[0].new_text, "");
    }

    #[test]
    fn skips_unused_import_remove_action_for_unrelated_diagnostic() {
        let text = "<?php\nuse App\\Models\\User;\nclass A {}\n";
        let uri = Url::parse("file:///tmp/a.php").expect("uri");
        let diagnostic = Diagnostic {
            range: Range::new(Position::new(1, 0), Position::new(1, 3)),
            severity: Some(DiagnosticSeverity::HINT),
            message: "Other diagnostic".to_string(),
            source: Some("vscode-ls-php".to_string()),
            ..Diagnostic::default()
        };

        let action = unused_import_remove_action(&diagnostic, text, &uri);
        assert!(action.is_none());
    }

    #[test]
    fn builds_duplicate_import_remove_action() {
        let text = "<?php\nuse App\\Models\\User;\nuse App\\Models\\User;\nclass A {}\n";
        let uri = Url::parse("file:///tmp/a.php").expect("uri");
        let diagnostic = Diagnostic {
            range: Range::new(Position::new(2, 15), Position::new(2, 19)),
            severity: Some(DiagnosticSeverity::HINT),
            message: "Duplicate import: User".to_string(),
            source: Some("vscode-ls-php".to_string()),
            ..Diagnostic::default()
        };

        let action = duplicate_import_remove_action(&diagnostic, text, &uri).expect("action");
        let CodeActionOrCommand::CodeAction(code_action) = action else {
            panic!("expected code action");
        };
        assert_eq!(code_action.title, "Remove duplicate import User");

        let edit = code_action.edit.expect("edit");
        let changes = edit.changes.expect("changes");
        let text_edits = changes.get(&uri).expect("uri edits");
        assert_eq!(text_edits.len(), 1);
        assert_eq!(text_edits[0].range.start, Position::new(2, 0));
        assert_eq!(text_edits[0].range.end, Position::new(3, 0));
    }

    #[test]
    fn skips_duplicate_import_remove_action_for_unrelated_diagnostic() {
        let text = "<?php\nuse App\\Models\\User;\nclass A {}\n";
        let uri = Url::parse("file:///tmp/a.php").expect("uri");
        let diagnostic = Diagnostic {
            range: Range::new(Position::new(1, 15), Position::new(1, 19)),
            severity: Some(DiagnosticSeverity::HINT),
            message: "Other diagnostic".to_string(),
            source: Some("vscode-ls-php".to_string()),
            ..Diagnostic::default()
        };

        let action = duplicate_import_remove_action(&diagnostic, text, &uri);
        assert!(action.is_none());
    }

    #[test]
    fn builds_operator_confusion_compare_action() {
        let uri = Url::parse("file:///tmp/test.php").expect("uri");
        let diagnostic = Diagnostic {
            range: Range::new(Position::new(1, 7), Position::new(1, 8)),
            severity: Some(DiagnosticSeverity::WARNING),
            message: "Suspicious assignment '=' in conditional expression".to_string(),
            source: Some("vscode-ls-php".to_string()),
            ..Diagnostic::default()
        };

        let action = operator_confusion_compare_action(&diagnostic, &uri).expect("action");
        let CodeActionOrCommand::CodeAction(code_action) = action else {
            panic!("expected code action");
        };
        assert_eq!(code_action.title, "Replace '=' with '=='");

        let edit = code_action.edit.expect("edit");
        let changes = edit.changes.expect("changes");
        let text_edits = changes.get(&uri).expect("uri edits");
        assert_eq!(text_edits.len(), 1);
        assert_eq!(text_edits[0].range, diagnostic.range);
        assert_eq!(text_edits[0].new_text, "==");
    }

    #[test]
    fn skips_operator_confusion_compare_action_for_unrelated_diagnostic() {
        let uri = Url::parse("file:///tmp/test.php").expect("uri");
        let diagnostic = Diagnostic {
            range: Range::new(Position::new(1, 7), Position::new(1, 8)),
            severity: Some(DiagnosticSeverity::WARNING),
            message: "Other diagnostic".to_string(),
            source: Some("vscode-ls-php".to_string()),
            ..Diagnostic::default()
        };

        let action = operator_confusion_compare_action(&diagnostic, &uri);
        assert!(action.is_none());
    }

    #[test]
    fn builds_unused_variable_remove_action() {
        let text = "<?php\nfunction run() {\n  $unused = 1;\n  return 1;\n}\n";
        let uri = Url::parse("file:///tmp/test.php").expect("uri");
        let diagnostic = Diagnostic {
            range: Range::new(Position::new(2, 2), Position::new(2, 9)),
            severity: Some(DiagnosticSeverity::HINT),
            message: "Unused variable: $unused".to_string(),
            source: Some("vscode-ls-php".to_string()),
            ..Diagnostic::default()
        };

        let action = unused_variable_remove_action(&diagnostic, text, &uri).expect("action");
        let CodeActionOrCommand::CodeAction(code_action) = action else {
            panic!("expected code action");
        };
        assert_eq!(code_action.title, "Remove unused variable $unused");

        let edit = code_action.edit.expect("edit");
        let changes = edit.changes.expect("changes");
        let text_edits = changes.get(&uri).expect("uri edits");
        assert_eq!(text_edits.len(), 1);
        assert_eq!(text_edits[0].range.start, Position::new(2, 0));
        assert_eq!(text_edits[0].range.end, Position::new(3, 0));
    }

    #[test]
    fn skips_unused_variable_remove_action_for_unrelated_diagnostic() {
        let text = "<?php\nfunction run() {\n  $unused = 1;\n}\n";
        let uri = Url::parse("file:///tmp/test.php").expect("uri");
        let diagnostic = Diagnostic {
            range: Range::new(Position::new(2, 2), Position::new(2, 9)),
            severity: Some(DiagnosticSeverity::HINT),
            message: "Other diagnostic".to_string(),
            source: Some("vscode-ls-php".to_string()),
            ..Diagnostic::default()
        };

        let action = unused_variable_remove_action(&diagnostic, text, &uri);
        assert!(action.is_none());
    }

    #[test]
    fn builds_brace_mismatch_remove_unexpected_closing_action() {
        let uri = Url::parse("file:///tmp/test.php").expect("uri");
        let diagnostic = Diagnostic {
            range: Range::new(Position::new(2, 0), Position::new(2, 1)),
            severity: Some(DiagnosticSeverity::ERROR),
            message: "Unexpected closing brace '}'".to_string(),
            source: Some("vscode-ls-php".to_string()),
            ..Diagnostic::default()
        };

        let action = brace_mismatch_fix_action(&diagnostic, "<?php\nfunction x() {}\n}", &uri)
            .expect("action");
        let CodeActionOrCommand::CodeAction(code_action) = action else {
            panic!("expected code action");
        };
        assert_eq!(code_action.title, "Remove unexpected '}'");
    }

    #[test]
    fn builds_brace_mismatch_add_missing_closing_action() {
        let uri = Url::parse("file:///tmp/test.php").expect("uri");
        let text = "<?php\nif (true) {\n    echo 'x';\n";
        let diagnostic = Diagnostic {
            range: Range::new(Position::new(1, 10), Position::new(1, 11)),
            severity: Some(DiagnosticSeverity::ERROR),
            message: "Unclosed opening brace '{'".to_string(),
            source: Some("vscode-ls-php".to_string()),
            ..Diagnostic::default()
        };

        let action = brace_mismatch_fix_action(&diagnostic, text, &uri).expect("action");
        let CodeActionOrCommand::CodeAction(code_action) = action else {
            panic!("expected code action");
        };
        assert_eq!(code_action.title, "Add missing closing '}'");

        let edit = code_action.edit.expect("edit");
        let changes = edit.changes.expect("changes");
        let text_edits = changes.get(&uri).expect("uri edits");
        assert_eq!(text_edits[0].range.start, Position::new(3, 0));
        assert_eq!(text_edits[0].range.end, Position::new(3, 0));
        assert_eq!(text_edits[0].new_text, "}\n");
    }

    #[test]
    fn builds_brace_mismatch_add_missing_closing_action_without_trailing_newline() {
        let uri = Url::parse("file:///tmp/test.php").expect("uri");
        let text = "<?php\nif (true) {";
        let diagnostic = Diagnostic {
            range: Range::new(Position::new(1, 10), Position::new(1, 11)),
            severity: Some(DiagnosticSeverity::ERROR),
            message: "Unclosed opening brace '{'".to_string(),
            source: Some("vscode-ls-php".to_string()),
            ..Diagnostic::default()
        };

        let action = brace_mismatch_fix_action(&diagnostic, text, &uri).expect("action");
        let CodeActionOrCommand::CodeAction(code_action) = action else {
            panic!("expected code action");
        };
        let edit = code_action.edit.expect("edit");
        let changes = edit.changes.expect("changes");
        let text_edits = changes.get(&uri).expect("uri edits");
        assert_eq!(text_edits[0].range.start, Position::new(1, 11));
        assert_eq!(text_edits[0].range.end, Position::new(1, 11));
        assert_eq!(text_edits[0].new_text, "\n}\n");
    }

    #[test]
    fn skips_brace_mismatch_action_for_unrelated_diagnostic() {
        let uri = Url::parse("file:///tmp/test.php").expect("uri");
        let diagnostic = Diagnostic {
            range: Range::new(Position::new(0, 0), Position::new(0, 1)),
            severity: Some(DiagnosticSeverity::ERROR),
            message: "Some other parse error".to_string(),
            source: Some("vscode-ls-php".to_string()),
            ..Diagnostic::default()
        };

        let action = brace_mismatch_fix_action(&diagnostic, "<?php\n", &uri);
        assert!(action.is_none());
    }

    #[test]
    fn builds_missing_return_type_add_action() {
        let text = "<?php\nfunction loadUser($id) { return $id; }\n";
        let uri = Url::parse("file:///tmp/test.php").expect("uri");
        let diagnostic = Diagnostic {
            range: Range::new(Position::new(1, 9), Position::new(1, 17)),
            severity: Some(DiagnosticSeverity::HINT),
            message: "Missing return type: loadUser()".to_string(),
            source: Some("vscode-ls-php".to_string()),
            ..Diagnostic::default()
        };

        let action = missing_return_type_add_action(&diagnostic, text, &uri).expect("action");
        let CodeActionOrCommand::CodeAction(code_action) = action else {
            panic!("expected code action");
        };
        assert_eq!(code_action.title, "Add return type to loadUser()");

        let edit = code_action.edit.expect("edit");
        let changes = edit.changes.expect("changes");
        let text_edits = changes.get(&uri).expect("uri edits");
        assert_eq!(text_edits[0].range.start, Position::new(1, 22));
        assert_eq!(text_edits[0].range.end, Position::new(1, 22));
        assert_eq!(text_edits[0].new_text, ": mixed");
    }

    #[test]
    fn skips_missing_return_type_add_action_when_return_type_exists() {
        let text = "<?php\nfunction loadUser($id): User { return $id; }\n";
        let uri = Url::parse("file:///tmp/test.php").expect("uri");
        let diagnostic = Diagnostic {
            range: Range::new(Position::new(1, 9), Position::new(1, 17)),
            severity: Some(DiagnosticSeverity::HINT),
            message: "Missing return type: loadUser()".to_string(),
            source: Some("vscode-ls-php".to_string()),
            ..Diagnostic::default()
        };

        let action = missing_return_type_add_action(&diagnostic, text, &uri);
        assert!(action.is_none());
    }

    #[test]
    fn builds_missing_return_type_add_action_for_multiline_signature() {
        let text = "<?php\nfunction loadUser(\n  $id\n) { return $id; }\n";
        let uri = Url::parse("file:///tmp/test.php").expect("uri");
        let diagnostic = Diagnostic {
            range: Range::new(Position::new(1, 9), Position::new(1, 17)),
            severity: Some(DiagnosticSeverity::HINT),
            message: "Missing return type: loadUser()".to_string(),
            source: Some("vscode-ls-php".to_string()),
            ..Diagnostic::default()
        };

        let action = missing_return_type_add_action(&diagnostic, text, &uri).expect("action");
        let CodeActionOrCommand::CodeAction(code_action) = action else {
            panic!("expected code action");
        };
        let edit = code_action.edit.expect("edit");
        let changes = edit.changes.expect("changes");
        let text_edits = changes.get(&uri).expect("uri edits");
        assert_eq!(text_edits[0].range.start, Position::new(3, 1));
        assert_eq!(text_edits[0].new_text, ": mixed");
    }

    #[test]
    fn skips_missing_return_type_add_action_when_multiline_return_type_exists() {
        let text = "<?php\nfunction loadUser(\n  $id\n): User { return $id; }\n";
        let uri = Url::parse("file:///tmp/test.php").expect("uri");
        let diagnostic = Diagnostic {
            range: Range::new(Position::new(1, 9), Position::new(1, 17)),
            severity: Some(DiagnosticSeverity::HINT),
            message: "Missing return type: loadUser()".to_string(),
            source: Some("vscode-ls-php".to_string()),
            ..Diagnostic::default()
        };

        let action = missing_return_type_add_action(&diagnostic, text, &uri);
        assert!(action.is_none());
    }

    #[test]
    fn detects_function_keyword_column_for_declaration() {
        let line = "public static function loadUser($id)";
        let col = function_keyword_column(line).expect("function column");
        assert_eq!(col, 14);
    }

    #[test]
    fn skips_function_keyword_column_for_commented_out_line() {
        let line = "// function fake()";
        assert!(function_keyword_column(line).is_none());
    }

    #[test]
    fn builds_missing_return_type_add_action_for_class_method() {
        let text = "<?php\nclass A {\n  function getData() { return []; }\n}\n";
        let uri = Url::parse("file:///tmp/test.php").expect("uri");
        let diagnostic = Diagnostic {
            range: Range::new(Position::new(2, 11), Position::new(2, 18)),
            severity: Some(DiagnosticSeverity::HINT),
            message: "Missing return type: getData()".to_string(),
            source: Some("vscode-ls-php".to_string()),
            ..Diagnostic::default()
        };

        let action = missing_return_type_add_action(&diagnostic, text, &uri).expect("action");
        let CodeActionOrCommand::CodeAction(code_action) = action else {
            panic!("expected code action");
        };
        let edit = code_action.edit.expect("edit");
        let changes = edit.changes.expect("changes");
        let text_edits = changes.get(&uri).expect("uri edits");
        assert_eq!(text_edits[0].new_text, ": mixed");
    }

    #[test]
    fn skips_missing_return_type_add_action_for_unrelated_diagnostic() {
        let text = "<?php\nfunction foo() { return 1; }\n";
        let uri = Url::parse("file:///tmp/test.php").expect("uri");
        let diagnostic = Diagnostic {
            range: Range::new(Position::new(1, 9), Position::new(1, 12)),
            severity: Some(DiagnosticSeverity::HINT),
            message: "Some other diagnostic".to_string(),
            source: Some("vscode-ls-php".to_string()),
            ..Diagnostic::default()
        };

        let action = missing_return_type_add_action(&diagnostic, text, &uri);
        assert!(action.is_none());
    }

    #[test]
    fn builds_var_dump_delete_action_for_final_line_without_trailing_newline() {
        let text = "<?php\nvar_dump($x);";
        let uri = Url::parse("file:///tmp/test.php").expect("uri");
        let diagnostic = Diagnostic {
            range: Range::new(Position::new(1, 0), Position::new(1, 8)),
            severity: Some(DiagnosticSeverity::HINT),
            message: "Avoid leaving debug output in committed code.".to_string(),
            source: Some("vscode-ls-php".to_string()),
            ..Diagnostic::default()
        };

        let action = var_dump_delete_action(&diagnostic, text, &uri).expect("action");
        let CodeActionOrCommand::CodeAction(code_action) = action else {
            panic!("expected code action");
        };

        let edit = code_action.edit.expect("edit");
        let changes = edit.changes.expect("changes");
        let text_edits = changes.get(&uri).expect("uri edits");
        assert_eq!(text_edits[0].range.start, Position::new(0, 5));
        assert_eq!(text_edits[0].range.end, Position::new(1, 13));
    }

    #[test]
    fn builds_unused_import_remove_action_for_final_line_without_trailing_newline() {
        let text = "<?php\nuse App\\Models\\User;";
        let uri = Url::parse("file:///tmp/test.php").expect("uri");
        let diagnostic = Diagnostic {
            range: Range::new(Position::new(1, 15), Position::new(1, 19)),
            severity: Some(DiagnosticSeverity::HINT),
            message: "Unused import: User".to_string(),
            source: Some("vscode-ls-php".to_string()),
            ..Diagnostic::default()
        };

        let action = unused_import_remove_action(&diagnostic, text, &uri).expect("action");
        let CodeActionOrCommand::CodeAction(code_action) = action else {
            panic!("expected code action");
        };

        let edit = code_action.edit.expect("edit");
        let changes = edit.changes.expect("changes");
        let text_edits = changes.get(&uri).expect("uri edits");
        assert_eq!(text_edits[0].range.start, Position::new(0, 5));
        assert_eq!(text_edits[0].range.end, Position::new(1, 20));
    }

    #[test]
    fn computes_delete_line_range_for_last_line_without_newline() {
        let text = "<?php\nuse App\\Models\\User;";
        let (start, end) = delete_line_range(text, 1).expect("range");
        assert_eq!(start, Position::new(0, 5));
        assert_eq!(end, Position::new(1, 20));
    }

    #[test]
    fn builds_var_dump_delete_action_for_single_line_file() {
        let text = "var_dump($x);";
        let uri = Url::parse("file:///tmp/test.php").expect("uri");
        let line_len = text.len() as u32;
        let diagnostic = Diagnostic {
            range: Range::new(Position::new(0, 0), Position::new(0, 8)),
            severity: Some(DiagnosticSeverity::HINT),
            message: "Avoid leaving debug output in committed code.".to_string(),
            source: Some("vscode-ls-php".to_string()),
            ..Diagnostic::default()
        };

        let action = var_dump_delete_action(&diagnostic, text, &uri).expect("action");
        let CodeActionOrCommand::CodeAction(code_action) = action else {
            panic!("expected code action");
        };
        let edit = code_action.edit.expect("edit");
        let changes = edit.changes.expect("changes");
        let text_edits = changes.get(&uri).expect("uri edits");
        assert_eq!(text_edits[0].range.start, Position::new(0, 0));
        assert_eq!(text_edits[0].range.end, Position::new(0, line_len));
    }

#[test]
fn builds_range_formatting_edit_for_selected_lines() {
    let text = "<?php\n  $x = 1;   \n\n\n  $y = 2;   \nreturn $x + $y;\n";
    let requested = Range::new(Position::new(1, 2), Position::new(4, 4));

    let edit = format_range_line_edit(text, requested).expect("edit");
    assert_eq!(edit.range.start, Position::new(1, 2));
    assert_eq!(edit.range.end, Position::new(4, 4));
    assert_eq!(edit.new_text, "  $x = 1;\n\n\n  $y = 2;   ");
}

    #[test]
    fn skips_range_formatting_when_selection_already_clean() {
        let text = "<?php\n$x = 1;\n$y = 2;\n";
        let requested = Range::new(Position::new(1, 0), Position::new(1, 7));

        let edit = format_range_line_edit(text, requested);
        assert!(edit.is_none());
    }

    #[test]
    fn formats_current_line_trailing_spaces() {
        let text = "<?php\n$x = 1;   \n$y = 2;\n";
        let edit = format_current_line_edit(text, 1).expect("edit");
        assert_eq!(edit.range.start, Position::new(1, 0));
        assert_eq!(edit.new_text, "$x = 1;");
    }

    #[test]
    fn skips_current_line_format_when_clean() {
        let text = "<?php\n$x = 1;\n";
        let edit = format_current_line_edit(text, 1);
        assert!(edit.is_none());
    }

    #[test]
    fn range_formatter_does_not_force_trailing_newline() {
        assert_eq!(format_range_text("$x = 1;"), "$x = 1;");
        assert_eq!(format_range_text("$x = 1;   \n$y = 2;   \n"), "$x = 1;\n$y = 2;");
    }

    #[test]
    fn builds_php_tag_insert_action_for_missing_opening_tag() {
        let text = "echo 'test';\n";
        let uri = Url::parse("file:///tmp/test.php").expect("uri");
        let diagnostic = Diagnostic {
            range: Range::new(Position::new(0, 0), Position::new(0, 0)),
            severity: Some(DiagnosticSeverity::WARNING),
            message: "PHP file should contain an opening '<?php' tag.".to_string(),
            source: Some("vscode-ls-php".to_string()),
            ..Diagnostic::default()
        };

        let action = php_tag_insert_action(&diagnostic, text, &uri).expect("action");
        let CodeActionOrCommand::CodeAction(code_action) = action else {
            panic!("expected code action");
        };
        assert_eq!(code_action.title, "Add opening '<?php' tag");

        let edit = code_action.edit.expect("edit");
        let changes = edit.changes.expect("changes");
        let text_edits = changes.get(&uri).expect("uri edits");
        assert_eq!(text_edits[0].range.start, Position::new(0, 0));
        assert_eq!(text_edits[0].range.end, Position::new(0, 0));
        assert_eq!(text_edits[0].new_text, "<?php\n");
    }

    #[test]
    fn skips_php_tag_insert_when_opening_tag_exists() {
        let text = "<?php\necho 'test';\n";
        let uri = Url::parse("file:///tmp/test.php").expect("uri");
        let diagnostic = Diagnostic {
            range: Range::new(Position::new(0, 0), Position::new(0, 0)),
            severity: Some(DiagnosticSeverity::WARNING),
            message: "PHP file should contain an opening '<?php' tag.".to_string(),
            source: Some("vscode-ls-php".to_string()),
            ..Diagnostic::default()
        };

        let action = php_tag_insert_action(&diagnostic, text, &uri);
        assert!(action.is_none());
    }

    #[test]
    fn builds_undefined_var_declare_action() {
        let text = "<?php\n    $x = $undefined;\n";
        let uri = Url::parse("file:///tmp/test.php").expect("uri");
        let diagnostic = Diagnostic {
            range: Range::new(Position::new(1, 9), Position::new(1, 19)),
            severity: Some(DiagnosticSeverity::WARNING),
            message: "Undefined variable: $undefined".to_string(),
            source: Some("vscode-ls-php".to_string()),
            ..Diagnostic::default()
        };

        let action = undefined_var_declare_action(&diagnostic, text, &uri).expect("action");
        let CodeActionOrCommand::CodeAction(code_action) = action else {
            panic!("expected code action");
        };
        assert_eq!(code_action.title, "Declare $undefined = null");

        let edit = code_action.edit.expect("edit");
        let changes = edit.changes.expect("changes");
        let text_edits = changes.get(&uri).expect("uri edits");
        assert_eq!(text_edits[0].range.start, Position::new(1, 0));
        assert_eq!(text_edits[0].new_text, "    $undefined = null;\n");
    }

    #[test]
    fn skips_undefined_var_declare_for_unrelated_message() {
        let text = "<?php\n$x = 1;\n";
        let uri = Url::parse("file:///tmp/test.php").expect("uri");
        let diagnostic = Diagnostic {
            range: Range::new(Position::new(1, 0), Position::new(1, 1)),
            severity: Some(DiagnosticSeverity::WARNING),
            message: "Other warning".to_string(),
            source: Some("vscode-ls-php".to_string()),
            ..Diagnostic::default()
        };

        let action = undefined_var_declare_action(&diagnostic, text, &uri);
        assert!(action.is_none());
    }

    #[test]
    fn selection_range_identifier_and_line_ranges() {
        let source = "<?php\necho $variable;";
        let position = Position::new(1, 7);

        let (ident, ident_range) =
            identifier_and_range_at_position(source, position).expect("identifier");
        assert_eq!(ident, "$variable");
        assert_eq!(ident_range.start, Position::new(1, 5));
        assert_eq!(ident_range.end, Position::new(1, 14));

        let line = source.lines().nth(1).expect("line");
        let line_range = Range::new(Position::new(1, 0), Position::new(1, line.chars().count() as u32));
        assert_eq!(line_range.start, Position::new(1, 0));
        assert_eq!(line_range.end, Position::new(1, 15));
    }

    #[test]
    fn selection_range_returns_none_for_non_identifier_position() {
        let source = "<?php\necho + 123;";
        let position = Position::new(1, 6);

        let result = identifier_and_range_at_position(source, position);
        assert!(result.is_none());
    }

    #[test]
    fn selection_ranges_return_line_range_for_non_identifier_position() {
        let source = "<?php\necho + 123;";
        let positions = vec![Position::new(1, 6)];

        let ranges = selection_ranges_for_positions(source, &positions);
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].range.start, Position::new(1, 0));
        assert_eq!(ranges[0].range.end, Position::new(1, 11));
        assert!(ranges[0].parent.is_none());
    }

    #[test]
    fn selection_ranges_keep_order_for_mixed_positions() {
        let source = "<?php\necho $variable + 123;";
        let positions = vec![Position::new(1, 7), Position::new(1, 15)];

        let ranges = selection_ranges_for_positions(source, &positions);
        assert_eq!(ranges.len(), 2);

        assert_eq!(ranges[0].range.start, Position::new(1, 5));
        assert_eq!(ranges[0].range.end, Position::new(1, 14));
        assert!(ranges[0].parent.is_some());

        assert_eq!(ranges[1].range.start, Position::new(1, 0));
        assert_eq!(ranges[1].range.end, Position::new(1, 21));
        assert!(ranges[1].parent.is_none());
    }

    #[test]
    fn selection_ranges_skip_out_of_bounds_lines() {
        let source = "<?php\necho $variable;";
        let positions = vec![Position::new(1, 7), Position::new(8, 0)];

        let ranges = selection_ranges_for_positions(source, &positions);
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].range.start, Position::new(1, 5));
        assert_eq!(ranges[0].range.end, Position::new(1, 14));
    }

    #[test]
    fn skips_parameter_hints_for_empty_call_and_trailing_comma() {
        let source = "<?php\nrun_test();\nrun_test($value,);\n";
        let map = HashMap::from([(
            "run_test".to_string(),
            vec!["mixed $first".to_string(), "mixed $second".to_string()],
        )]);

        let empty_call_hints = collect_parameter_inlay_hints_for_range(source, 1, 1, &map);
        assert!(empty_call_hints.is_empty());

        let trailing_comma_hints = collect_parameter_inlay_hints_for_range(source, 2, 2, &map);
        assert_eq!(trailing_comma_hints.len(), 1);
        assert_eq!(trailing_comma_hints[0].2, "first:");
    }

    #[test]
    fn collects_parameter_hints_when_first_argument_is_nested_call() {
        let source = "<?php\nrun_test(inner(1, 2), 3);\n";
        let map = HashMap::from([(
            "run_test".to_string(),
            vec!["mixed $left".to_string(), "mixed $right".to_string()],
        )]);

        let hints = collect_parameter_inlay_hints_for_range(source, 1, 1, &map);
        assert_eq!(hints.len(), 2);
        assert_eq!(hints[0].2, "left:");
        assert_eq!(hints[1].2, "right:");
    }
