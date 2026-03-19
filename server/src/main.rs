use anyhow::Result;
use serde_json::json;
use std::collections::{HashMap, HashSet, BTreeMap};
use std::fs;
use tokio::io::{stdin, stdout};
use tokio::sync::RwLock;
use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::{
    CallHierarchyIncomingCall, CallHierarchyIncomingCallsParams, CallHierarchyItem,
    CallHierarchyOutgoingCall, CallHierarchyOutgoingCallsParams, CallHierarchyPrepareParams,
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams,
    CodeLens, CodeLensOptions, CodeLensParams, Command,
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionParams, CompletionResponse,
    DocumentFormattingParams,
    DocumentOnTypeFormattingOptions, DocumentOnTypeFormattingParams,
    DocumentRangeFormattingParams,
    DocumentLink, DocumentLinkOptions, DocumentLinkParams,
    DidChangeWatchedFilesParams,
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DidSaveTextDocumentParams, Diagnostic, DiagnosticSeverity, Hover, HoverContents, HoverParams,
    DocumentHighlight, DocumentHighlightKind, DocumentHighlightParams, InitializeParams,
    FoldingRange, FoldingRangeParams,
    InlayHint, InlayHintKind, InlayHintLabel, InlayHintParams,
    InitializeResult, Location, MarkedString, MessageType, OneOf, Position,
    PrepareRenameResponse, Range, ReferenceParams, RenameParams, TextDocumentPositionParams,
    SignatureHelp, SignatureHelpOptions, SignatureHelpParams, SignatureInformation,
    ParameterInformation, ParameterLabel,
    SemanticToken, SemanticTokenType, SemanticTokens, SemanticTokensFullOptions,
    SemanticTokensLegend, SemanticTokensOptions, SemanticTokensParams,
    SemanticTokensResult, SemanticTokensServerCapabilities,
    SelectionRange, SelectionRangeParams, SelectionRangeProviderCapability,
    TextEdit, WorkspaceEdit,
    ServerCapabilities, SymbolInformation, SymbolKind, TextDocumentSyncCapability,
    TextDocumentSyncKind, Url, WorkspaceSymbolParams, GotoDefinitionParams,
    GotoDefinitionResponse,
};
use tower_lsp::{Client, LanguageServer, LspService, Server};

mod formatting;
mod fs_scan;
mod diagnostics;

use formatting::{
    document_end_position,
    format_blade_directive_spacing,
    format_current_line_edit,
    format_document,
    format_range_line_edit,
    format_range_text,
    looks_like_blade_template,
};
use diagnostics::{
    detect_brace_mismatch,
    detect_duplicate_imports,
    detect_missing_return_types,
    detect_operator_confusion,
    detect_undefined_function_calls,
    detect_undefined_function_calls_with_known,
    detect_undefined_methods,
    detect_undefined_variables,
    detect_unused_imports,
    detect_unused_variables,
    extract_first_variable_name,
    is_builtin_variable,
    variable_occurrences_in_line,
};
use fs_scan::{collect_php_files, is_blade_uri, is_php_uri, should_skip_dir};

#[derive(Clone)]
struct PhpSymbol {
    name: String,
    namespace: Option<String>,
    kind: SymbolKind,
    parameters: Vec<String>,
    return_type: Option<String>,
    range: Range,
}

impl PhpSymbol {
    fn fqn(&self) -> String {
        if let Some(namespace) = &self.namespace {
            if !namespace.is_empty() {
                return format!("{}\\{}", namespace, self.name);
            }
        }
        self.name.clone()
    }
}

struct Backend {
    client: Client,
    documents: RwLock<HashMap<Url, String>>,
    symbols: RwLock<HashMap<Url, Vec<PhpSymbol>>>,
    workspace_folders: RwLock<Vec<Url>>,
    open_documents: RwLock<HashSet<Url>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CompletionContextKind {
    General,
    UseStatement,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum NamespaceDeclaration {
    Inline(String),
    Block(String),
    Global,
}

impl Backend {
    async fn update_document(&self, uri: Url, text: String) {
        {
            let mut documents = self.documents.write().await;
            documents.insert(uri.clone(), text.clone());
        }

        let new_symbols = extract_symbols(&text);
        {
            let mut symbols = self.symbols.write().await;
            symbols.insert(uri.clone(), new_symbols);
        }

        self.publish_diagnostics(&uri, &text).await;
    }

    async fn clear_document_diagnostics(&self, uri: &Url) {
        self.client.publish_diagnostics(uri.clone(), Vec::new(), None).await;
    }

    async fn set_workspace_folders(&self, roots: Vec<Url>) {
        let mut folders = self.workspace_folders.write().await;
        *folders = roots;
    }

    async fn index_workspace(&self) {
        let roots = {
            let folders = self.workspace_folders.read().await;
            folders.clone()
        };

        let mut indexed_files = 0usize;
        for root in roots {
            let Ok(root_path) = root.to_file_path() else {
                continue;
            };

            let mut php_files = Vec::new();
            collect_php_files(&root_path, &mut php_files);

            for file_path in php_files {
                let Ok(text) = fs::read_to_string(&file_path) else {
                    continue;
                };

                let Ok(uri) = Url::from_file_path(&file_path) else {
                    continue;
                };

                self.update_document(uri, text).await;
                indexed_files += 1;
            }
        }

        let _ = self
            .client
            .log_message(
                MessageType::INFO,
                format!("vscode-ls-php indexed {indexed_files} PHP files"),
            )
            .await;
    }

    async fn publish_diagnostics(&self, uri: &Url, text: &str) {
        let mut diagnostics: Vec<Diagnostic> = Vec::new();

        if !is_blade_uri(uri) && !text.contains("<?php") {
            diagnostics.push(Diagnostic {
                range: Range::new(Position::new(0, 0), Position::new(0, 0)),
                severity: Some(DiagnosticSeverity::WARNING),
                message: "PHP file should contain an opening '<?php' tag.".to_string(),
                source: Some("vscode-ls-php".to_string()),
                ..Diagnostic::default()
            });
        }

        for (line_idx, line) in text.lines().enumerate() {
            if let Some(column) = line.find("var_dump(") {
                diagnostics.push(Diagnostic {
                    range: Range::new(
                        Position::new(line_idx as u32, column as u32),
                        Position::new(line_idx as u32, (column + 8) as u32),
                    ),
                    severity: Some(DiagnosticSeverity::HINT),
                    message: "Avoid leaving debug output in committed code.".to_string(),
                    source: Some("vscode-ls-php".to_string()),
                    ..Diagnostic::default()
                });
            }
        }

        diagnostics.extend(detect_brace_mismatch(text));
        diagnostics.extend(detect_operator_confusion(text));
        let known_meta_functions = self.collect_workspace_phpstorm_meta_functions().await;
        diagnostics.extend(detect_undefined_function_calls_with_known(
            text,
            &known_meta_functions,
        ));
        diagnostics.extend(detect_undefined_methods(text));
        diagnostics.extend(detect_undefined_variables(text));
        diagnostics.extend(detect_unused_variables(text));
        diagnostics.extend(detect_unused_imports(text));
        diagnostics.extend(detect_duplicate_imports(text));
        diagnostics.extend(detect_missing_return_types(text));

        self.client
            .publish_diagnostics(uri.clone(), diagnostics, None)
            .await;
    }

    async fn find_definition_location(
        &self,
        current_uri: &Url,
        queries: &[String],
    ) -> Option<Location> {
        let index = self.symbols.read().await;

        if let Some(symbols) = index.get(current_uri) {
            for query in queries {
                if let Some(symbol) = symbols.iter().find(|symbol| symbol_matches_query(symbol, query)) {
                    return Some(Location {
                        uri: current_uri.clone(),
                        range: symbol.range,
                    });
                }
            }
        }

        for query in queries {
            for (symbol_uri, symbols) in index.iter() {
                if *symbol_uri == *current_uri {
                    continue;
                }

                if let Some(symbol) = symbols.iter().find(|symbol| symbol_matches_query(symbol, query)) {
                    return Some(Location {
                        uri: symbol_uri.clone(),
                        range: symbol.range,
                    });
                }
            }
        }

        None
    }

    async fn find_type_definition_location(
        &self,
        current_uri: &Url,
        queries: &[String],
    ) -> Option<Location> {
        let index = self.symbols.read().await;
        find_type_definition_in_index(&index, current_uri, queries)
    }

    async fn resolve_target_fqn(&self, current_uri: &Url, queries: &[String]) -> Option<String> {
        let index = self.symbols.read().await;

        if let Some(symbols) = index.get(current_uri) {
            for query in queries {
                if let Some(symbol) = symbols.iter().find(|symbol| symbol_matches_query(symbol, query)) {
                    return Some(symbol.fqn());
                }
            }
        }

        for query in queries {
            for (symbol_uri, symbols) in index.iter() {
                if *symbol_uri == *current_uri {
                    continue;
                }

                if let Some(symbol) = symbols.iter().find(|symbol| symbol_matches_query(symbol, query)) {
                    return Some(symbol.fqn());
                }
            }
        }

        None
    }

    async fn collect_workspace_phpstorm_meta_functions(&self) -> HashSet<String> {
        let docs = self.documents.read().await;
        let mut known = HashSet::new();
        for (uri, text) in docs.iter() {
            let path = uri.path().to_ascii_lowercase();
            if !path.ends_with(".php") {
                continue;
            }
            if !path.contains(".phpstorm.meta") {
                continue;
            }
            known.extend(extract_phpstorm_meta_override_function_names(text));
        }
        known
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> LspResult<InitializeResult> {
        let mut roots: Vec<Url> = Vec::new();
        if let Some(folders) = params.workspace_folders {
            roots.extend(folders.into_iter().map(|folder| folder.uri));
        } else if let Some(root) = params.root_uri {
            roots.push(root);
        }
        self.set_workspace_folders(roots).await;

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
                hover_provider: Some(tower_lsp::lsp_types::HoverProviderCapability::Simple(true)),
                completion_provider: Some(CompletionOptions {
                    resolve_provider: Some(true),
                    ..CompletionOptions::default()
                }),
                inlay_hint_provider: Some(OneOf::Left(true)),
                signature_help_provider: Some(SignatureHelpOptions {
                    trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
                    retrigger_characters: Some(vec![",".to_string()]),
                    work_done_progress_options: Default::default(),
                }),
                definition_provider: Some(OneOf::Left(true)),
                type_definition_provider: Some(
                    tower_lsp::lsp_types::TypeDefinitionProviderCapability::Simple(true),
                ),
                call_hierarchy_provider: Some(
                    tower_lsp::lsp_types::CallHierarchyServerCapability::Simple(true),
                ),
                implementation_provider: Some(
                    tower_lsp::lsp_types::ImplementationProviderCapability::Simple(true),
                ),
                references_provider: Some(OneOf::Left(true)),
                rename_provider: Some(OneOf::Left(true)),
                document_highlight_provider: Some(OneOf::Left(true)),
                code_action_provider: Some(tower_lsp::lsp_types::CodeActionProviderCapability::Simple(true)),
                code_lens_provider: Some(CodeLensOptions {
                    resolve_provider: Some(false),
                }),
                semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
                    SemanticTokensOptions {
                        work_done_progress_options: Default::default(),
                        legend: SemanticTokensLegend {
                            token_types: vec![
                                SemanticTokenType::VARIABLE,
                                SemanticTokenType::FUNCTION,
                                SemanticTokenType::CLASS,
                                SemanticTokenType::INTERFACE,
                                SemanticTokenType::ENUM,
                                SemanticTokenType::NAMESPACE,
                            ],
                            token_modifiers: Vec::new(),
                        },
                        range: None,
                        full: Some(SemanticTokensFullOptions::Bool(true)),
                    },
                )),
                document_link_provider: Some(DocumentLinkOptions {
                    resolve_provider: Some(false),
                    work_done_progress_options: Default::default(),
                }),
                document_formatting_provider: Some(OneOf::Left(true)),
                document_range_formatting_provider: Some(OneOf::Left(true)),
                document_on_type_formatting_provider: Some(DocumentOnTypeFormattingOptions {
                    first_trigger_character: ";".to_string(),
                    more_trigger_character: Some(vec!["}".to_string()]),
                }),
                folding_range_provider: Some(
                    tower_lsp::lsp_types::FoldingRangeProviderCapability::Simple(true),
                ),
                selection_range_provider: Some(SelectionRangeProviderCapability::Simple(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                ..ServerCapabilities::default()
            },
            ..InitializeResult::default()
        })
    }

    async fn initialized(&self, _: tower_lsp::lsp_types::InitializedParams) {
        let _ = self
            .client
            .log_message(MessageType::INFO, "vscode-ls-php-server initialized")
            .await;

        self.index_workspace().await;
    }

    async fn shutdown(&self) -> LspResult<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        {
            let mut open = self.open_documents.write().await;
            open.insert(params.text_document.uri.clone());
        }
        self
            .update_document(params.text_document.uri, params.text_document.text)
            .await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        if let Some(change) = params.content_changes.into_iter().next() {
            self.update_document(params.text_document.uri, change.text).await;
        }
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        if let Some(text) = params.text {
            self.update_document(params.text_document.uri, text).await;
            return;
        }

        let uri = params.text_document.uri;
        let current = {
            let documents = self.documents.read().await;
            documents.get(&uri).cloned()
        };
        if let Some(text) = current {
            self.publish_diagnostics(&uri, &text).await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        {
            let mut open = self.open_documents.write().await;
            open.remove(&params.text_document.uri);
        }
        self.clear_document_diagnostics(&params.text_document.uri).await;
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        for change in params.changes {
            let uri = change.uri;
            if !is_php_uri(&uri) {
                continue;
            }

            match change.typ {
                tower_lsp::lsp_types::FileChangeType::DELETED => {
                    {
                        let mut docs = self.documents.write().await;
                        docs.remove(&uri);
                    }
                    {
                        let mut symbols = self.symbols.write().await;
                        symbols.remove(&uri);
                    }
                    self.clear_document_diagnostics(&uri).await;
                }
                tower_lsp::lsp_types::FileChangeType::CREATED
                | tower_lsp::lsp_types::FileChangeType::CHANGED => {
                    let is_open = {
                        let open = self.open_documents.read().await;
                        open.contains(&uri)
                    };
                    if is_open {
                        continue;
                    }

                    let Ok(path) = uri.to_file_path() else {
                        continue;
                    };
                    let Ok(text) = fs::read_to_string(path) else {
                        continue;
                    };
                    self.update_document(uri, text).await;
                }
                _ => {}
            }
        }
    }

    async fn hover(&self, params: HoverParams) -> LspResult<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let docs_snapshot = {
            let docs = self.documents.read().await;
            docs.clone()
        };

        let context = {
            docs_snapshot.get(&uri).and_then(|text| {
                identifier_at_position(text, position).map(|identifier| {
                    let queries = resolve_symbol_queries(text, position, &identifier);
                    (identifier, queries)
                })
            })
        };

        let Some((identifier, queries)) = context else {
            return Ok(None);
        };

        let index = self.symbols.read().await;
        if let Some((symbol_uri, symbol)) = find_symbol_location_for_queries(&uri, &index, &queries) {
            let template_params = docs_snapshot
                .get(symbol_uri)
                .and_then(|text| get_nearby_docblock_for_line(text, symbol.range.start.line as usize))
                .map(|docblock| extract_template_params_from_docblock(&docblock))
                .unwrap_or_default();

            return Ok(Some(Hover {
                contents: HoverContents::Scalar(MarkedString::String(
                    format_symbol_for_hover_with_templates(symbol, &template_params),
                )),
                range: Some(symbol.range),
            }));
        }

        if let Some(url) = php_manual_function_url(&identifier) {
            return Ok(Some(Hover {
                contents: HoverContents::Scalar(MarkedString::String(format!(
                    "PHP manual: [{}]({})",
                    identifier.trim_start_matches('\\'),
                    url
                ))),
                range: None,
            }));
        }

        Ok(None)
    }

    async fn completion(&self, params: CompletionParams) -> LspResult<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;

        let docs_snapshot = {
            let docs = self.documents.read().await;
            docs.clone()
        };
        let text = docs_snapshot.get(&uri).cloned();

        let is_blade_template = is_blade_uri(&uri)
            || text
                .as_deref()
                .map(looks_like_blade_template)
                .unwrap_or(false);
        let route_context = text
            .as_deref()
            .and_then(|content| laravel_string_completion_context(content, position, "route"));
        let config_context = text
            .as_deref()
            .and_then(|content| laravel_string_completion_context(content, position, "config"));

        let context_kind = text
            .as_deref()
            .map(|t| completion_context_kind(t, position))
            .unwrap_or(CompletionContextKind::General);

        let prefix = {
            text.as_deref()
                .and_then(|content| identifier_prefix_at_position(content, position))
                .unwrap_or_default()
                .to_lowercase()
        };

        let mut keyword_items = vec![
            "class",
            "interface",
            "trait",
            "enum",
            "function",
            "const",
            "public",
            "private",
            "protected",
            "static",
            "namespace",
            "use",
            "return",
            "if",
            "else",
            "foreach",
        ];
        if is_blade_template {
            keyword_items.extend(blade_directive_keywords());
        }

        let mut seen: HashSet<String> = HashSet::new();
        let mut scored_items: Vec<(i32, CompletionItem)> = Vec::new();

        let static_context = text
            .as_deref()
            .and_then(|content| static_member_completion_context(content, position));
        let instance_context = text
            .as_deref()
            .and_then(|content| instance_member_completion_context(content, position));

        for keyword in keyword_items.into_iter().chain(php_magic_constants()) {
            if (!prefix.is_empty() && !keyword.starts_with(&prefix)) || !seen.insert(keyword.to_string()) {
                continue;
            }
            let item = CompletionItem {
                label: keyword.to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                detail: Some("PHP keyword".to_string()),
                ..CompletionItem::default()
            };
            let score = completion_score(&item.label, &prefix, false, item.kind, context_kind);
            scored_items.push((score, item));
        }

        if let Some(content) = text.as_deref() {
            for variable in extract_local_variables_before_position(content, position) {
                let variable_lower = variable.to_lowercase();
                if (!prefix.is_empty() && !variable_lower.starts_with(&prefix))
                    || !seen.insert(variable.clone())
                {
                    continue;
                }

                let item = CompletionItem {
                    label: variable,
                    kind: Some(CompletionItemKind::VARIABLE),
                    detail: Some("Local variable".to_string()),
                    ..CompletionItem::default()
                };
                let score = completion_score(&item.label, &prefix, true, item.kind, context_kind);
                scored_items.push((score, item));
            }
        }

        for facade in laravel_facade_names() {
            let facade_lower = facade.to_lowercase();
            if (!prefix.is_empty() && !facade_lower.starts_with(&prefix))
                || !seen.insert(facade.to_string())
            {
                continue;
            }

            let item = CompletionItem {
                label: facade.to_string(),
                kind: Some(CompletionItemKind::CLASS),
                detail: Some("Laravel facade".to_string()),
                ..CompletionItem::default()
            };
            let score = completion_score(&item.label, &prefix, false, item.kind, context_kind) + 8;
            scored_items.push((score, item));
        }

        for helper in laravel_helper_functions() {
            let helper_lower = helper.to_lowercase();
            if (!prefix.is_empty() && !helper_lower.starts_with(&prefix))
                || !seen.insert(helper.to_string())
            {
                continue;
            }

            let item = CompletionItem {
                label: helper.to_string(),
                kind: Some(CompletionItemKind::FUNCTION),
                detail: Some("Laravel helper".to_string()),
                ..CompletionItem::default()
            };
            let score = completion_score(&item.label, &prefix, false, item.kind, context_kind) + 8;
            scored_items.push((score, item));
        }

        if route_context.is_some() {
            for route_name in collect_laravel_route_names(&docs_snapshot) {
                if (!prefix.is_empty() && !route_name.to_lowercase().starts_with(&prefix))
                    || !seen.insert(route_name.clone())
                {
                    continue;
                }

                let item = CompletionItem {
                    label: route_name,
                    kind: Some(CompletionItemKind::VALUE),
                    detail: Some("Laravel route name".to_string()),
                    ..CompletionItem::default()
                };
                let score = completion_score(&item.label, &prefix, false, item.kind, context_kind) + 22;
                scored_items.push((score, item));
            }
        }

        if config_context.is_some() {
            for config_key in collect_laravel_config_keys(&docs_snapshot) {
                if (!prefix.is_empty() && !config_key.to_lowercase().starts_with(&prefix))
                    || !seen.insert(config_key.clone())
                {
                    continue;
                }

                let item = CompletionItem {
                    label: config_key,
                    kind: Some(CompletionItemKind::VALUE),
                    detail: Some("Laravel config key".to_string()),
                    ..CompletionItem::default()
                };
                let score = completion_score(&item.label, &prefix, false, item.kind, context_kind) + 22;
                scored_items.push((score, item));
            }
        }

        let index = self.symbols.read().await;
        if let (Some(content), Some(class_name)) = (text.as_deref(), static_context.as_deref()) {
            let mut queries = resolve_symbol_queries(content, position, class_name);
            if !queries.iter().any(|q| q == class_name) {
                queries.push(class_name.to_string());
            }

            if let Some((target_uri, class_symbol)) =
                find_symbol_location_for_queries(&uri, &index, &queries)
            {
                if is_type_symbol_kind(class_symbol.kind) {
                    let target_text = if *target_uri == uri {
                        Some(content.to_string())
                    } else {
                        let docs = self.documents.read().await;
                        docs.get(target_uri).cloned()
                    };

                    if let Some(target_text) = target_text {
                        for member in collect_class_member_entries(&target_text, class_symbol) {
                            if !member.is_static {
                                continue;
                            }
                            let label = member.label;
                            let kind = member.kind;
                            let member_type = member.type_hint;
                            let lowercase = label.to_lowercase();
                            if (!prefix.is_empty() && !lowercase.starts_with(&prefix))
                                || !seen.insert(label.clone())
                            {
                                continue;
                            }

                            let item = CompletionItem {
                                label: label.clone(),
                                kind: Some(kind),
                                detail: Some(match member_type.as_deref() {
                                    Some(type_name) => format!("Class member: {}", type_name),
                                    None => "Class member".to_string(),
                                }),
                                data: Some(json!({
                                    "memberOf": class_symbol.fqn(),
                                    "member": label,
                                    "memberKind": if kind == CompletionItemKind::METHOD { "method" } else { "property" },
                                    "memberType": member_type
                                })),
                                ..CompletionItem::default()
                            };
                            let score = completion_score(&item.label, &prefix, *target_uri == uri, item.kind, context_kind) + 15;
                            scored_items.push((score, item));
                        }
                    }
                }
            }
        }

        if static_context.is_some() {
            for method in laravel_eloquent_static_methods() {
                if (!prefix.is_empty() && !method.starts_with(&prefix))
                    || !seen.insert(method.to_string())
                {
                    continue;
                }

                let item = CompletionItem {
                    label: method.to_string(),
                    kind: Some(CompletionItemKind::METHOD),
                    detail: Some("Eloquent static method".to_string()),
                    ..CompletionItem::default()
                };
                let score = completion_score(&item.label, &prefix, false, item.kind, context_kind) + 12;
                scored_items.push((score, item));
            }
        }

        if let (Some(content), Some(variable_name)) = (text.as_deref(), instance_context.as_deref()) {
            if let Some(class_name) = infer_variable_class_before_position(content, position, variable_name) {
                let mut queries = resolve_symbol_queries(content, position, &class_name);
                if !queries.iter().any(|q| q == &class_name) {
                    queries.push(class_name.clone());
                }

                if let Some((target_uri, class_symbol)) =
                    find_symbol_location_for_queries(&uri, &index, &queries)
                {
                    if is_type_symbol_kind(class_symbol.kind) {
                        let template_mapping = {
                            let raw_var_type = infer_variable_type_annotation_before_position(
                                content,
                                position,
                                variable_name,
                            );

                            let mut mapping = BTreeMap::new();
                            if let Some(raw_var_type) = raw_var_type {
                                if let Some((raw_base, generic_args)) = parse_generic_type_instance(&raw_var_type) {
                                    let resolved_base = normalize_type_name_for_inference(&raw_base)
                                        .unwrap_or_else(|| raw_base.trim_start_matches('\\').to_string());
                                    let class_fqn = class_symbol.fqn();
                                    let class_short = class_symbol.name.as_str();
                                    let resolved_base_short =
                                        resolved_base.rsplit_once('\\').map(|(_, s)| s).unwrap_or(&resolved_base);
                                    if resolved_base == class_fqn
                                        || resolved_base == class_short
                                        || resolved_base_short == class_short
                                    {
                                        if let Some(class_source) = if *target_uri == uri {
                                            Some(content.to_string())
                                        } else {
                                            let docs = self.documents.read().await;
                                            docs.get(target_uri).cloned()
                                        } {
                                            let template_params =
                                                get_nearby_docblock_for_line(
                                                    &class_source,
                                                    class_symbol.range.start.line as usize,
                                                )
                                                .map(|doc| extract_template_params_from_docblock(&doc))
                                                .unwrap_or_default();
                                            for (idx, param_name) in template_params.iter().enumerate() {
                                                if let Some(arg) = generic_args.get(idx) {
                                                    mapping.insert(param_name.clone(), arg.clone());
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            mapping
                        };

                        let target_text = if *target_uri == uri {
                            Some(content.to_string())
                        } else {
                            let docs = self.documents.read().await;
                            docs.get(target_uri).cloned()
                        };

                        if let Some(target_text) = target_text {
                            for member in collect_class_member_entries(&target_text, class_symbol) {
                                if member.is_static {
                                    continue;
                                }
                                let label = member.label;
                                let kind = member.kind;
                                let member_type = member
                                    .type_hint
                                    .as_deref()
                                    .map(|raw| apply_template_substitution(raw, &template_mapping));
                                let lowercase = label.to_lowercase();
                                if (!prefix.is_empty() && !lowercase.starts_with(&prefix))
                                    || !seen.insert(label.clone())
                                {
                                    continue;
                                }

                                let item = CompletionItem {
                                    label: label.clone(),
                                    kind: Some(kind),
                                    detail: Some(match member_type.as_deref() {
                                        Some(type_name) => format!("Instance member: {}", type_name),
                                        None => "Instance member".to_string(),
                                    }),
                                    data: Some(json!({
                                        "memberOf": class_symbol.fqn(),
                                        "member": label,
                                        "memberKind": if kind == CompletionItemKind::METHOD { "method" } else { "property" },
                                        "memberType": member_type
                                    })),
                                    ..CompletionItem::default()
                                };
                                let score = completion_score(&item.label, &prefix, *target_uri == uri, item.kind, context_kind) + 12;
                                scored_items.push((score, item));
                            }
                        }
                    }
                }
            }
        }

        if instance_context.is_some() {
            for method in laravel_eloquent_instance_methods() {
                if (!prefix.is_empty() && !method.starts_with(&prefix))
                    || !seen.insert(method.to_string())
                {
                    continue;
                }

                let item = CompletionItem {
                    label: method.to_string(),
                    kind: Some(CompletionItemKind::METHOD),
                    detail: Some("Eloquent instance method".to_string()),
                    ..CompletionItem::default()
                };
                let score = completion_score(&item.label, &prefix, false, item.kind, context_kind) + 10;
                scored_items.push((score, item));
            }
        }

        for (symbol_uri, symbols) in index.iter() {
            let local = *symbol_uri == uri;
            for symbol in symbols {
                let label = symbol_completion_label(symbol, context_kind);
                let lowercase = label.to_lowercase();
                if (!prefix.is_empty() && !lowercase.starts_with(&prefix))
                    || !seen.insert(label.clone())
                {
                    continue;
                }

                let item = CompletionItem {
                    label,
                    kind: Some(completion_kind_from_symbol(symbol.kind)),
                    detail: Some("Workspace symbol".to_string()),
                    data: Some(json!(symbol.fqn())),
                    ..CompletionItem::default()
                };
                let score = completion_score(&item.label, &prefix, local, item.kind, context_kind);
                scored_items.push((score, item));
            }
        }

        scored_items.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| a.1.label.to_lowercase().cmp(&b.1.label.to_lowercase()))
        });
        let items = scored_items.into_iter().map(|(_, item)| item).collect::<Vec<_>>();

        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn completion_resolve(&self, params: CompletionItem) -> LspResult<CompletionItem> {
        let index = self.symbols.read().await;
        Ok(resolve_completion_item(params, &index))
    }

    async fn prepare_call_hierarchy(
        &self,
        params: CallHierarchyPrepareParams,
    ) -> LspResult<Option<Vec<CallHierarchyItem>>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let context = {
            let docs = self.documents.read().await;
            docs.get(&uri).and_then(|text| {
                let name = identifier_at_position(text, position)?;
                let queries = resolve_symbol_queries(text, position, &name);
                Some((name, queries))
            })
        };
        let Some((name, mut queries)) = context else {
            return Ok(None);
        };
        if !queries.iter().any(|q| q == &name) {
            queries.push(name);
        }

        let index = self.symbols.read().await;
        let Some((symbol_uri, symbol)) = find_symbol_location_for_queries(&uri, &index, &queries) else {
            return Ok(None);
        };

        Ok(Some(vec![call_hierarchy_item_for_symbol(symbol_uri, symbol)]))
    }

    async fn incoming_calls(
        &self,
        params: CallHierarchyIncomingCallsParams,
    ) -> LspResult<Option<Vec<CallHierarchyIncomingCall>>> {
        let item = params.item;
        let target = item
            .data
            .as_ref()
            .and_then(|v| v.as_str())
            .map(|s| s.trim_start_matches('\\').to_string())
            .unwrap_or_else(|| item.name.clone());

        let docs_snapshot = {
            let docs = self.documents.read().await;
            docs.clone()
        };
        let symbols_snapshot = {
            let symbols = self.symbols.read().await;
            symbols.clone()
        };

        let mut grouped: HashMap<String, (CallHierarchyItem, Vec<Range>)> = HashMap::new();

        for (doc_uri, text) in docs_snapshot {
            let terms = if target.contains('\\') {
                search_terms_for_target_in_document(&target, &text, true)
            } else {
                vec![target.clone()]
            };

            let mut hit_ranges = Vec::new();
            for term in terms {
                hit_ranges.extend(call_ranges_for_name(&text, &term));
            }
            if hit_ranges.is_empty() {
                continue;
            }

            let Some(symbols) = symbols_snapshot.get(&doc_uri) else {
                continue;
            };

            for hit in hit_ranges {
                let Some(caller) = enclosing_function_symbol(symbols, hit) else {
                    continue;
                };
                if caller.fqn() == target {
                    continue;
                }
                let key = format!("{}::{}", doc_uri, caller.fqn());
                let entry = grouped
                    .entry(key)
                    .or_insert_with(|| (call_hierarchy_item_for_symbol(&doc_uri, caller), Vec::new()));
                entry.1.push(hit);
            }
        }

        if grouped.is_empty() {
            return Ok(None);
        }

        let mut incoming = grouped
            .into_values()
            .map(|(from, from_ranges)| CallHierarchyIncomingCall { from, from_ranges })
            .collect::<Vec<_>>();
        incoming.sort_by(|a, b| a.from.name.to_lowercase().cmp(&b.from.name.to_lowercase()));

        Ok(Some(incoming))
    }

    async fn outgoing_calls(
        &self,
        params: CallHierarchyOutgoingCallsParams,
    ) -> LspResult<Option<Vec<CallHierarchyOutgoingCall>>> {
        let item = params.item;

        let docs_snapshot = {
            let docs = self.documents.read().await;
            docs.clone()
        };
        let symbols_snapshot = {
            let symbols = self.symbols.read().await;
            symbols.clone()
        };

        let source_uri = item.uri;
        let Some(source_text) = docs_snapshot.get(&source_uri) else {
            return Ok(None);
        };
        let Some(source_symbols) = symbols_snapshot.get(&source_uri) else {
            return Ok(None);
        };

        let source_fqn = item
            .data
            .as_ref()
            .and_then(|v| v.as_str())
            .map(|s| s.trim_start_matches('\\').to_string());
        let source_symbol = if let Some(fqn) = source_fqn {
            source_symbols.iter().find(|symbol| symbol.fqn() == fqn)
        } else {
            source_symbols.iter().find(|symbol| symbol.name == item.name)
        };
        let Some(source_symbol) = source_symbol else {
            return Ok(None);
        };

        let mut grouped: HashMap<String, (CallHierarchyItem, Vec<Range>)> = HashMap::new();
        for (target_uri, symbols) in symbols_snapshot.iter() {
            for target_symbol in symbols.iter().filter(|s| s.kind == SymbolKind::FUNCTION) {
                let terms = search_terms_for_target_in_document(&target_symbol.fqn(), source_text, true);
                let mut hit_ranges = Vec::new();
                for term in terms {
                    hit_ranges.extend(
                        call_ranges_for_name(source_text, &term)
                            .into_iter()
                            .filter(|range| {
                                is_position_within_range(range.start, source_symbol.range)
                                    && is_position_within_range(range.end, source_symbol.range)
                            }),
                    );
                }

                if hit_ranges.is_empty() {
                    continue;
                }

                let key = format!("{}::{}", target_uri, target_symbol.fqn());
                let entry = grouped.entry(key).or_insert_with(|| {
                    (call_hierarchy_item_for_symbol(target_uri, target_symbol), Vec::new())
                });
                entry.1.extend(hit_ranges);
            }
        }

        if grouped.is_empty() {
            return Ok(None);
        }

        let mut outgoing = grouped
            .into_values()
            .map(|(to, from_ranges)| CallHierarchyOutgoingCall { to, from_ranges })
            .collect::<Vec<_>>();
        outgoing.sort_by(|a, b| a.to.name.to_lowercase().cmp(&b.to.name.to_lowercase()));

        Ok(Some(outgoing))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> LspResult<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let context = {
            let docs = self.documents.read().await;
            docs.get(&uri).and_then(|text| {
                let name = identifier_at_position(text, position)?;
                let queries = resolve_symbol_queries(text, position, &name);
                Some((name, queries))
            })
        };
        let Some((name, mut queries)) = context else {
            return Ok(None);
        };

        if !queries.iter().any(|q| q == &name) {
            queries.push(name);
        }

        if let Some(location) = self.find_definition_location(&uri, &queries).await {
            return Ok(Some(GotoDefinitionResponse::Scalar(location)));
        }

        Ok(None)
    }

    async fn goto_type_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> LspResult<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let context = {
            let docs = self.documents.read().await;
            docs.get(&uri).and_then(|text| {
                let name = identifier_at_position(text, position)?;
                let queries = resolve_symbol_queries(text, position, &name);
                Some((name, queries))
            })
        };
        let Some((name, mut queries)) = context else {
            return Ok(None);
        };

        if !queries.iter().any(|q| q == &name) {
            queries.push(name);
        }

        if let Some(location) = self.find_type_definition_location(&uri, &queries).await {
            return Ok(Some(GotoDefinitionResponse::Scalar(location)));
        }

        Ok(None)
    }

    async fn goto_implementation(
        &self,
        params: GotoDefinitionParams,
    ) -> LspResult<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let context = {
            let docs = self.documents.read().await;
            docs.get(&uri).and_then(|text| {
                let name = identifier_at_position(text, position)?;
                let queries = resolve_symbol_queries(text, position, &name);
                Some((name, queries))
            })
        };
        let Some((name, mut queries)) = context else {
            return Ok(None);
        };
        if !queries.iter().any(|q| q == &name) {
            queries.push(name.clone());
        }

        let target_fqn = self.resolve_target_fqn(&uri, &queries).await;
        let Some(target_fqn) = target_fqn else {
            return Ok(None);
        };

        let docs_snapshot = {
            let docs = self.documents.read().await;
            docs.clone()
        };
        let symbols_snapshot = {
            let symbols = self.symbols.read().await;
            symbols.clone()
        };

        let implementations = collect_class_implementation_locations(
            &target_fqn,
            &docs_snapshot,
            &symbols_snapshot,
        );

        if implementations.is_empty() {
            return Ok(None);
        }

        Ok(Some(GotoDefinitionResponse::Array(implementations)))
    }

    async fn signature_help(&self, params: SignatureHelpParams) -> LspResult<Option<SignatureHelp>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let docs_snapshot = {
            let docs = self.documents.read().await;
            docs.clone()
        };
        let text = docs_snapshot.get(&uri).cloned();
        let Some(text) = text else {
            return Ok(None);
        };

        let Some((function_name, active_parameter)) = function_call_context(&text, position) else {
            return Ok(None);
        };

        let index = self.symbols.read().await;
        let mut candidate: Option<(Url, PhpSymbol)> = None;

        if let Some(symbols) = index.get(&uri) {
            if let Some(sym) = symbols.iter().find(|s| s.name == function_name && s.kind == SymbolKind::FUNCTION) {
                candidate = Some((uri.clone(), sym.clone()));
            }
        }

        if candidate.is_none() {
            for (symbol_uri, symbols) in index.iter() {
                if let Some(sym) = symbols.iter().find(|s| s.name == function_name && s.kind == SymbolKind::FUNCTION) {
                    candidate = Some((symbol_uri.clone(), sym.clone()));
                    break;
                }
            }
        }

        let Some((symbol_uri, symbol)) = candidate else {
            return Ok(None);
        };

        let template_params = docs_snapshot
            .get(&symbol_uri)
            .and_then(|source| get_nearby_docblock_for_line(source, symbol.range.start.line as usize))
            .map(|docblock| extract_template_params_from_docblock(&docblock))
            .unwrap_or_default();
        let display_name = symbol_display_name_with_templates(&symbol.name, &template_params);
        let label = function_signature_label(&display_name, &symbol.parameters, symbol.return_type.as_deref());
        let params_info = symbol
            .parameters
            .iter()
            .map(|p| ParameterInformation {
                label: ParameterLabel::Simple(p.clone()),
                documentation: None,
            })
            .collect::<Vec<_>>();
        let capped_active_parameter = active_parameter.min(symbol.parameters.len().saturating_sub(1));

        Ok(Some(SignatureHelp {
            signatures: vec![SignatureInformation {
                label,
                documentation: None,
                parameters: Some(params_info),
                active_parameter: Some(capped_active_parameter as u32),
            }],
            active_signature: Some(0),
            active_parameter: Some(capped_active_parameter as u32),
        }))
    }

    async fn references(&self, params: ReferenceParams) -> LspResult<Option<Vec<Location>>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;

        let context = {
            let docs = self.documents.read().await;
            docs.get(&uri).and_then(|text| {
                let name = identifier_at_position(text, position)?;
                let queries = resolve_symbol_queries(text, position, &name);
                Some((name, queries))
            })
        };
        let Some((name, queries)) = context else {
            return Ok(None);
        };

        let target_fqn = self.resolve_target_fqn(&uri, &queries).await;

        let docs_snapshot = {
            let docs = self.documents.read().await;
            docs.clone()
        };

        let mut locations: Vec<Location> = Vec::new();
        for (doc_uri, text) in docs_snapshot {
            let terms = if let Some(fqn) = &target_fqn {
                search_terms_for_target_in_document(fqn, &text, true)
            } else {
                vec![name.clone()]
            };

            for term in terms {
                for range in find_identifier_ranges(&text, &term) {
                    locations.push(Location {
                        uri: doc_uri.clone(),
                        range,
                    });
                }
            }
        }

        locations.sort_by(|a, b| {
            a.uri
                .as_str()
                .cmp(b.uri.as_str())
                .then_with(|| a.range.start.line.cmp(&b.range.start.line))
                .then_with(|| a.range.start.character.cmp(&b.range.start.character))
        });
        locations.dedup_by(|a, b| {
            a.uri == b.uri
                && a.range.start == b.range.start
                && a.range.end == b.range.end
        });

        if !params.context.include_declaration {
            let defs = self.symbols.read().await;
            locations.retain(|loc| {
                !defs
                    .get(&loc.uri)
                    .map(|symbols| {
                        symbols.iter().any(|symbol| {
                            symbol.range == loc.range
                                && if let Some(fqn) = &target_fqn {
                                    symbol.fqn() == *fqn
                                } else {
                                    symbol.name == name
                                }
                        })
                    })
                    .unwrap_or(false)
            });
        }

        Ok(Some(locations))
    }

    async fn document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> LspResult<Option<Vec<DocumentHighlight>>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let text = {
            let docs = self.documents.read().await;
            docs.get(&uri).cloned()
        };
        let Some(text) = text else {
            return Ok(None);
        };

        if !is_code_position(&text, position) {
            return Ok(Some(Vec::new()));
        }

        let name = identifier_at_position(&text, position);
        let Some(name) = name else {
            return Ok(Some(Vec::new()));
        };

        let ranges = find_identifier_ranges(&text, &name);
        let highlights = ranges
            .into_iter()
            .map(|range| DocumentHighlight {
                range,
                kind: Some(DocumentHighlightKind::TEXT),
            })
            .collect::<Vec<_>>();

        Ok(Some(highlights))
    }

    async fn selection_range(
        &self,
        params: SelectionRangeParams,
    ) -> LspResult<Option<Vec<SelectionRange>>> {
        let uri = params.text_document.uri;
        let text = {
            let docs = self.documents.read().await;
            docs.get(&uri).cloned()
        };

        let Some(text) = text else {
            return Ok(Some(Vec::new()));
        };

        let ranges = selection_ranges_for_positions(&text, &params.positions);

        Ok(Some(ranges))
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> LspResult<Option<PrepareRenameResponse>> {
        let uri = params.text_document.uri;
        let position = params.position;

        let result = {
            let docs = self.documents.read().await;
            docs.get(&uri).and_then(|text| {
                if !is_code_position(text, position) {
                    return None;
                }
                identifier_and_range_at_position(text, position)
            })
        };

        let Some((name, range)) = result else {
            return Ok(None);
        };

        Ok(Some(PrepareRenameResponse::RangeWithPlaceholder {
            range,
            placeholder: name,
        }))
    }

    async fn rename(&self, params: RenameParams) -> LspResult<Option<WorkspaceEdit>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let mut new_name = params.new_name.trim().to_string();

        let context = {
            let docs = self.documents.read().await;
            docs.get(&uri).and_then(|text| {
                if !is_code_position(text, position) {
                    return None;
                }
                let old_name = identifier_at_position(text, position)?;
                let queries = resolve_symbol_queries(text, position, &old_name);
                Some((old_name, queries))
            })
        };

        let Some((old_name, queries)) = context else {
            return Ok(None);
        };

        if old_name.starts_with('$') && !new_name.starts_with('$') {
            new_name = format!("${new_name}");
        }

        if new_name.is_empty() || !is_valid_identifier_name(&new_name) {
            return Ok(None);
        }

        if old_name == new_name {
            return Ok(Some(WorkspaceEdit::default()));
        }

        let target_fqn = self.resolve_target_fqn(&uri, &queries).await;

        let docs_snapshot = {
            let docs = self.documents.read().await;
            docs.clone()
        };

        let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
        for (doc_uri, text) in docs_snapshot {
            let terms = if let Some(fqn) = &target_fqn {
                search_terms_for_target_in_document(fqn, &text, false)
            } else {
                vec![old_name.clone()]
            };

            let mut edits = Vec::new();
            for term in terms {
                edits.extend(find_identifier_ranges(&text, &term).into_iter().map(|range| TextEdit {
                    range,
                    new_text: new_name.clone(),
                }));
            }

            if edits.is_empty() {
                continue;
            }

            edits.sort_by(|a, b| {
                a.range
                    .start
                    .line
                    .cmp(&b.range.start.line)
                    .then_with(|| a.range.start.character.cmp(&b.range.start.character))
            });
            edits.dedup_by(|a, b| a.range == b.range);

            changes.insert(doc_uri, edits);
        }

        if changes.is_empty() {
            return Ok(None);
        }

        Ok(Some(WorkspaceEdit {
            changes: Some(changes),
            document_changes: None,
            change_annotations: None,
        }))
    }

    async fn code_action(
        &self,
        params: CodeActionParams,
    ) -> LspResult<Option<Vec<CodeActionOrCommand>>> {
        let uri = params.text_document.uri;
        let position = params.range.start;

        let text = {
            let docs = self.documents.read().await;
            docs.get(&uri).cloned()
        };
        let Some(text) = text else {
            return Ok(None);
        };

        let mut actions = Vec::new();
        for diagnostic in params.context.diagnostics.iter() {
            if let Some(action) = var_dump_delete_action(diagnostic, &text, &uri) {
                actions.push(action);
            }
            if let Some(action) = php_tag_insert_action(diagnostic, &text, &uri) {
                actions.push(action);
            }
            if let Some(action) = undefined_var_declare_action(diagnostic, &text, &uri) {
                actions.push(action);
            }
            if let Some(action) = unused_import_remove_action(diagnostic, &text, &uri) {
                actions.push(action);
            }
            if let Some(action) = duplicate_import_remove_action(diagnostic, &text, &uri) {
                actions.push(action);
            }
            if let Some(action) = operator_confusion_compare_action(diagnostic, &uri) {
                actions.push(action);
            }
            if let Some(action) = unused_variable_remove_action(diagnostic, &text, &uri) {
                actions.push(action);
            }
            if let Some(action) = brace_mismatch_fix_action(diagnostic, &text, &uri) {
                actions.push(action);
            }
            if let Some(action) = missing_return_type_add_action(diagnostic, &text, &uri) {
                actions.push(action);
            }
        }

        if !is_code_position(&text, position) {
            return if actions.is_empty() {
                Ok(None)
            } else {
                Ok(Some(actions))
            };
        }

        let ident = identifier_at_position(&text, position);
        let Some(name) = ident else {
            return if actions.is_empty() {
                Ok(None)
            } else {
                Ok(Some(actions))
            };
        };

        let short = name.trim_start_matches('\\').trim_start_matches('$').to_string();
        if short.is_empty() || short.contains('\\') || !looks_like_type_name(&short) {
            return if actions.is_empty() {
                Ok(None)
            } else {
                Ok(Some(actions))
            };
        }

        let existing_imports = parse_use_aliases(&text);

        let current_ns = first_namespace_in_text(&text);
        let insertion = find_use_insertion_position(&text);

        let index = self.symbols.read().await;
        let mut candidate_fqns = HashSet::new();
        for symbols in index.values() {
            for symbol in symbols {
                if symbol.name != short {
                    continue;
                }
                if !matches!(symbol.kind, SymbolKind::CLASS | SymbolKind::INTERFACE | SymbolKind::MODULE | SymbolKind::ENUM) {
                    continue;
                }

                let fqn = symbol.fqn();
                let (ns, _) = split_fqn(&fqn);
                if ns == current_ns {
                    continue;
                }
                if existing_imports.values().any(|v| v == &fqn) {
                    continue;
                }
                candidate_fqns.insert(fqn);
            }
        }

        if candidate_fqns.is_empty() {
            return if actions.is_empty() {
                Ok(None)
            } else {
                Ok(Some(actions))
            };
        }

        let mut fqns = candidate_fqns.into_iter().collect::<Vec<_>>();
        fqns.sort();

        for fqn in fqns.into_iter().take(5) {
            let Some((new_text, title)) = import_action_for_fqn(&fqn, &existing_imports) else {
                continue;
            };

            let edit = WorkspaceEdit {
                changes: Some(HashMap::from([(
                    uri.clone(),
                    vec![TextEdit {
                        range: Range::new(insertion, insertion),
                        new_text,
                    }],
                )])),
                document_changes: None,
                change_annotations: None,
            };

            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                title,
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: None,
                edit: Some(edit),
                command: None,
                is_preferred: Some(true),
                disabled: None,
                data: None,
            }));
        }

        if actions.is_empty() {
            Ok(None)
        } else {
            Ok(Some(actions))
        }
    }

    async fn code_lens(&self, params: CodeLensParams) -> LspResult<Option<Vec<CodeLens>>> {
        let uri = params.text_document.uri;

        let symbols = {
            let index = self.symbols.read().await;
            index.get(&uri).cloned().unwrap_or_default()
        };
        if symbols.is_empty() {
            return Ok(Some(Vec::new()));
        }

        let docs_snapshot = {
            let docs = self.documents.read().await;
            docs.clone()
        };

        let mut lenses = Vec::new();
        for symbol in symbols {
            if !matches!(
                symbol.kind,
                SymbolKind::CLASS
                    | SymbolKind::INTERFACE
                    | SymbolKind::MODULE
                    | SymbolKind::ENUM
                    | SymbolKind::FUNCTION
            ) {
                continue;
            }

            let references = reference_locations_for_symbol(&symbol, &uri, &docs_snapshot);
            let count = references.len();
            let title = reference_count_title(count);

            let references_json = references
                .iter()
                .map(|loc| {
                    json!({
                        "uri": loc.uri.as_str(),
                        "range": {
                            "start": {
                                "line": loc.range.start.line,
                                "character": loc.range.start.character,
                            },
                            "end": {
                                "line": loc.range.end.line,
                                "character": loc.range.end.character,
                            }
                        }
                    })
                })
                .collect::<Vec<_>>();

            lenses.push(CodeLens {
                range: symbol.range,
                command: Some(Command {
                    title,
                    command: "editor.action.showReferences".to_string(),
                    arguments: Some(vec![
                        json!(uri.as_str()),
                        json!({
                            "line": symbol.range.start.line,
                            "character": symbol.range.start.character,
                        }),
                        json!(references_json),
                    ]),
                }),
                data: None,
            });
        }

        Ok(Some(lenses))
    }

    async fn document_link(
        &self,
        params: DocumentLinkParams,
    ) -> LspResult<Option<Vec<DocumentLink>>> {
        let uri = params.text_document.uri;
        let text = {
            let docs = self.documents.read().await;
            docs.get(&uri).cloned()
        };

        let Some(text) = text else {
            return Ok(None);
        };

        Ok(Some(detect_http_urls(&text)))
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> LspResult<Option<SemanticTokensResult>> {
        let uri = params.text_document.uri;
        let text = {
            let docs = self.documents.read().await;
            docs.get(&uri).cloned()
        };

        let Some(text) = text else {
            return Ok(None);
        };

        let tokens = tokenize_php_document(&text);
        let data = compress_tokens_to_semantic_data(tokens);

        Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: None,
            data,
        })))
    }

    async fn folding_range(&self, params: FoldingRangeParams) -> LspResult<Option<Vec<FoldingRange>>> {
        let uri = params.text_document.uri;
        let text = {
            let docs = self.documents.read().await;
            docs.get(&uri).cloned()
        };

        let Some(text) = text else {
            return Ok(Some(Vec::new()));
        };

        Ok(Some(collect_folding_ranges(&text)))
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> LspResult<Option<Vec<InlayHint>>> {
        let uri = params.text_document.uri;
        let text = {
            let docs = self.documents.read().await;
            docs.get(&uri).cloned()
        };
        let Some(text) = text else {
            return Ok(Some(Vec::new()));
        };

        let symbols_snapshot = {
            let symbols = self.symbols.read().await;
            symbols.clone()
        };
        let function_param_map = build_function_parameter_map(&symbols_snapshot);

        let start_line = params.range.start.line;
        let end_line = params.range.end.line;
        let hint_tuples = collect_parameter_inlay_hints_for_range(
            &text,
            start_line,
            end_line,
            &function_param_map,
        );
        let mut hints = hint_tuples
            .into_iter()
            .map(|(line, character, label)| InlayHint {
                position: Position::new(line, character),
                label: InlayHintLabel::String(label),
                kind: Some(InlayHintKind::PARAMETER),
                text_edits: None,
                tooltip: None,
                padding_left: Some(false),
                padding_right: Some(true),
                data: None,
            })
            .collect::<Vec<_>>();

        hints.extend(collect_return_type_inlay_hints_for_range(
            &symbols_snapshot,
            &uri,
            start_line,
            end_line,
        ));

        Ok(Some(hints))
    }

    async fn formatting(
        &self,
        params: DocumentFormattingParams,
    ) -> LspResult<Option<Vec<TextEdit>>> {
        let uri = params.text_document.uri;
        let original_text = {
            let docs = self.documents.read().await;
            docs.get(&uri).cloned()
        };

        let Some(original_text) = original_text else {
            return Ok(None);
        };

        let formatted_text = format_document(&original_text);
        if formatted_text == original_text {
            return Ok(Some(Vec::new()));
        }

        Ok(Some(vec![TextEdit {
            // Use a full-document replacement range to avoid newline edge-case ambiguity.
            range: Range::new(Position::new(0, 0), Position::new(u32::MAX, u32::MAX)),
            new_text: formatted_text,
        }]))
    }

    async fn range_formatting(
        &self,
        params: DocumentRangeFormattingParams,
    ) -> LspResult<Option<Vec<TextEdit>>> {
        let uri = params.text_document.uri;
        let original_text = {
            let docs = self.documents.read().await;
            docs.get(&uri).cloned()
        };

        let Some(original_text) = original_text else {
            return Ok(None);
        };

        let Some(edit) = format_range_line_edit(&original_text, params.range) else {
            return Ok(Some(Vec::new()));
        };

        Ok(Some(vec![edit]))
    }

    async fn on_type_formatting(
        &self,
        params: DocumentOnTypeFormattingParams,
    ) -> LspResult<Option<Vec<TextEdit>>> {
        let uri = params.text_document_position.text_document.uri;
        let line = params.text_document_position.position.line;

        let original_text = {
            let docs = self.documents.read().await;
            docs.get(&uri).cloned()
        };

        let Some(original_text) = original_text else {
            return Ok(None);
        };

        let Some(edit) = format_current_line_edit(&original_text, line) else {
            return Ok(Some(Vec::new()));
        };

        Ok(Some(vec![edit]))
    }

    #[allow(deprecated)]
    async fn document_symbol(
        &self,
        params: tower_lsp::lsp_types::DocumentSymbolParams,
    ) -> LspResult<Option<tower_lsp::lsp_types::DocumentSymbolResponse>> {
        let symbols = {
            let index = self.symbols.read().await;
            index
                .get(&params.text_document.uri)
                .cloned()
                .unwrap_or_default()
        };

        let response = symbols
            .into_iter()
            .map(|symbol| SymbolInformation {
                name: symbol.name,
                kind: symbol.kind,
                tags: None,
                deprecated: None,
                location: tower_lsp::lsp_types::Location {
                    uri: params.text_document.uri.clone(),
                    range: symbol.range,
                },
                container_name: None,
            })
            .collect::<Vec<_>>();

        Ok(Some(tower_lsp::lsp_types::DocumentSymbolResponse::Flat(
            response,
        )))
    }

    #[allow(deprecated)]
    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> LspResult<Option<Vec<SymbolInformation>>> {
        let query = params.query.to_lowercase();
        let index = self.symbols.read().await;

        let mut response: Vec<(i32, SymbolInformation)> = Vec::new();
        for (uri, symbols) in index.iter() {
            for symbol in symbols {
                let Some(score) = workspace_symbol_score(symbol, &query) else {
                    continue;
                };
                response.push((
                    score,
                    SymbolInformation {
                        name: symbol.name.clone(),
                        kind: symbol.kind,
                        tags: None,
                        deprecated: None,
                        location: tower_lsp::lsp_types::Location {
                            uri: uri.clone(),
                            range: symbol.range,
                        },
                        container_name: None,
                    },
                ));
            }
        }

        response.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| a.1.name.to_lowercase().cmp(&b.1.name.to_lowercase()))
                .then_with(|| a.1.location.uri.as_str().cmp(b.1.location.uri.as_str()))
        });

        Ok(Some(
            response
                .into_iter()
                .take(500)
                .map(|(_, item)| item)
                .collect(),
        ))
    }
}

fn workspace_symbol_score(symbol: &PhpSymbol, query: &str) -> Option<i32> {
    if !workspace_symbol_matches_query(symbol, query) {
        return None;
    }

    let query = query.trim();
    if query.is_empty() {
        return Some(1);
    }

    let name = symbol.name.to_lowercase();
    let fqn = symbol.fqn().to_lowercase();
    let kind = workspace_symbol_kind_name(symbol.kind);
    let mut score = 10;

    if name == query {
        score += 40;
    } else if name.starts_with(query) {
        score += 25;
    }

    if fqn == query {
        score += 35;
    } else if fqn.starts_with(query) {
        score += 20;
    }

    if kind == query {
        score += 5;
    }

    Some(score)
}

fn workspace_symbol_matches_query(symbol: &PhpSymbol, query: &str) -> bool {
    let query = query.trim();
    if query.is_empty() {
        return true;
    }

    let name = symbol.name.to_lowercase();
    let fqn = symbol.fqn().to_lowercase();
    let kind = workspace_symbol_kind_name(symbol.kind);

    for token in query.split_whitespace() {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        if !name.contains(token) && !fqn.contains(token) && !kind.contains(token) {
            return false;
        }
    }

    true
}

fn workspace_symbol_kind_name(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::CLASS => "class",
        SymbolKind::FUNCTION => "function",
        SymbolKind::INTERFACE => "interface",
        SymbolKind::MODULE => "trait",
        SymbolKind::ENUM => "enum",
        SymbolKind::CONSTANT => "constant",
        SymbolKind::VARIABLE => "variable",
        _ => "symbol",
    }
}

fn extract_symbols(text: &str) -> Vec<PhpSymbol> {
    let mut symbols = Vec::new();
    let mut current_namespace: Option<String> = None;
    let mut block_namespace_depth: Option<i32> = None;
    let mut brace_depth: i32 = 0;
    let lines = text.lines().collect::<Vec<_>>();

    for (line_idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let line_open = line.chars().filter(|ch| *ch == '{').count() as i32;
        let line_close = line.chars().filter(|ch| *ch == '}').count() as i32;

        if let Some(namespace) = parse_namespace_declaration(trimmed) {
            match namespace {
                NamespaceDeclaration::Inline(ns) => {
                    current_namespace = Some(ns);
                    block_namespace_depth = None;
                }
                NamespaceDeclaration::Block(ns) => {
                    current_namespace = Some(ns);
                    let depth_after_line = (brace_depth + line_open - line_close).max(0);
                    block_namespace_depth = Some(if depth_after_line > brace_depth {
                        depth_after_line
                    } else {
                        brace_depth + 1
                    });
                }
                NamespaceDeclaration::Global => {
                    current_namespace = None;
                    block_namespace_depth = None;
                }
            }
        }

        let class_like = [
            ("class", SymbolKind::CLASS),
            ("interface", SymbolKind::INTERFACE),
            ("trait", SymbolKind::MODULE),
            ("enum", SymbolKind::ENUM),
        ];

        for (keyword, kind) in class_like {
            if let Some(name) = token_after_keyword(trimmed, keyword) {
                if let Some(column) = line.find(&name) {
                    let end_column = column + name.len();
                    symbols.push(PhpSymbol {
                        name,
                        namespace: current_namespace.clone(),
                        kind,
                        parameters: Vec::new(),
                        return_type: None,
                        range: Range::new(
                            Position::new(line_idx as u32, column as u32),
                            Position::new(line_idx as u32, end_column as u32),
                        ),
                    });
                }
                continue;
            }
        }

        if let Some(name) = token_after_keyword(trimmed, "function") {
            if let Some(column) = line.find(&name) {
                let end_column = column + name.len();
                let signature = collect_function_declaration_signature(&lines, line_idx);
                symbols.push(PhpSymbol {
                    name,
                    namespace: current_namespace.clone(),
                    kind: SymbolKind::FUNCTION,
                    parameters: parse_function_parameters(&signature),
                    return_type: parse_function_return_type(&signature).or_else(|| {
                        get_nearby_docblock(&lines, line_idx)
                            .and_then(|docblock| extract_return_from_docblock(&docblock))
                    }),
                    range: Range::new(
                        Position::new(line_idx as u32, column as u32),
                        Position::new(line_idx as u32, end_column as u32),
                    ),
                });
            }
            continue;
        }

        if let Some(name) = token_after_keyword(trimmed, "const") {
            if let Some(column) = line.find(&name) {
                let end_column = column + name.len();
                symbols.push(PhpSymbol {
                    name,
                    namespace: current_namespace.clone(),
                    kind: SymbolKind::CONSTANT,
                    parameters: Vec::new(),
                    return_type: None,
                    range: Range::new(
                        Position::new(line_idx as u32, column as u32),
                        Position::new(line_idx as u32, end_column as u32),
                    ),
                });
            }
        }

        brace_depth = (brace_depth + line_open - line_close).max(0);
        if let Some(start_depth) = block_namespace_depth {
            if brace_depth < start_depth {
                current_namespace = None;
                block_namespace_depth = None;
            }
        }
    }

    symbols
}

fn get_nearby_docblock(lines: &[&str], function_line_idx: usize) -> Option<String> {
    const MAX_DOCBLOCK_LOOKBACK: usize = 10;

    if function_line_idx == 0 {
        return None;
    }

    let mut idx = function_line_idx as isize - 1;
    let min_idx = function_line_idx.saturating_sub(MAX_DOCBLOCK_LOOKBACK) as isize;

    while idx >= min_idx && lines[idx as usize].trim().is_empty() {
        idx -= 1;
    }
    if idx < min_idx {
        return None;
    }

    let end_line = lines[idx as usize].trim();
    if !end_line.ends_with("*/") {
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

fn get_nearby_docblock_for_line(text: &str, line_idx: usize) -> Option<String> {
    let lines = text.lines().collect::<Vec<_>>();
    get_nearby_docblock(&lines, line_idx)
}

fn extract_return_from_docblock(comment_text: &str) -> Option<String> {
    for tag in ["@psalm-return", "@phpstan-return", "@return"] {
        if let Some(offset) = comment_text.find(tag) {
            let tail = comment_text.get(offset + tag.len()..)?.trim_start();
            if let Some(parsed) = parse_type_annotation_prefix(tail) {
                return Some(parsed);
            }
        }
    }
    None
}

fn extract_template_params_from_docblock(comment_text: &str) -> Vec<String> {
    let tags = [
        "@template",
        "@template-covariant",
        "@psalm-template",
        "@psalm-template-covariant",
        "@phpstan-template",
        "@phpstan-template-covariant",
    ];

    let mut templates = Vec::new();
    let mut seen = HashSet::new();

    for line in comment_text.lines() {
        let cleaned = line.trim().trim_start_matches('*').trim_start();
        for tag in tags {
            if let Some(rest) = cleaned.strip_prefix(tag) {
                if let Some(name) = parse_template_name_prefix(rest.trim_start()) {
                    if seen.insert(name.clone()) {
                        templates.push(name);
                    }
                }
            }
        }
    }

    templates
}

fn parse_template_name_prefix(input: &str) -> Option<String> {
    let mut chars = input.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic() && first != '_' {
        return None;
    }

    let mut out = String::new();
    out.push(first);
    for ch in chars {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch);
            continue;
        }
        break;
    }

    Some(out)
}

fn extract_phpstorm_meta_override_function_names(text: &str) -> HashSet<String> {
    let mut names = HashSet::new();

    let mut rest = text;
    while let Some(idx) = rest.find("override(") {
        let after = &rest[(idx + "override(".len())..];
        let after_trim = after.trim_start();

        let candidate = after_trim
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '\\' || *ch == ':')
            .collect::<String>();

        if !candidate.is_empty() && !candidate.contains("::") {
            let canonical = candidate.trim_start_matches('\\').to_ascii_lowercase();
            if canonical.contains('\\') {
                names.insert(canonical);
            }
        }

        rest = after;
    }

    names
}

fn parse_namespace_declaration(trimmed_line: &str) -> Option<NamespaceDeclaration> {
    if !trimmed_line.starts_with("namespace") {
        return None;
    }

    let rest = trimmed_line.trim_start_matches("namespace").trim_start();
    if rest.starts_with(';') {
        return Some(NamespaceDeclaration::Global);
    }

    let namespace: String = rest
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '\\')
        .collect();

    if namespace.is_empty() {
        None
    } else {
        let tail = rest.get(namespace.len()..).unwrap_or("").trim_start();
        if tail.starts_with('{') {
            Some(NamespaceDeclaration::Block(namespace))
        } else {
            Some(NamespaceDeclaration::Inline(namespace))
        }
    }
}

fn parse_use_aliases(text: &str) -> HashMap<String, String> {
    let mut aliases = HashMap::new();

    for line in text.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("use ") {
            continue;
        }

        // Ignore closure captures like: function () use ($a) {}
        if trimmed.contains("use (") || trimmed.contains("use(") {
            continue;
        }

        let mut rest = trimmed.trim_start_matches("use ").trim();
        if rest.starts_with("function ") || rest.starts_with("const ") {
            continue;
        }

        if let Some(idx) = rest.find(';') {
            rest = &rest[..idx];
        }

        parse_use_clause_entries(rest, &mut aliases);
    }

    aliases
}

fn parse_use_clause_entries(clause: &str, aliases: &mut HashMap<String, String>) {
    for entry in split_use_entries(clause) {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }

        if let Some(open) = entry.find('{') {
            let Some(close) = entry.rfind('}') else {
                continue;
            };
            if close <= open {
                continue;
            }

            let prefix = entry[..open].trim().trim_end_matches('\\').trim();
            let inner = &entry[(open + 1)..close];
            if prefix.is_empty() {
                continue;
            }

            for part in split_use_entries(inner) {
                let part = part.trim();
                if part.is_empty() {
                    continue;
                }
                let full = format!("{}\\{}", prefix, part);
                parse_single_use_entry(&full, aliases);
            }
            continue;
        }

        parse_single_use_entry(entry, aliases);
    }
}

fn split_use_entries(input: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut brace_depth = 0i32;

    for ch in input.chars() {
        match ch {
            '{' => {
                brace_depth += 1;
                current.push(ch);
            }
            '}' => {
                brace_depth -= 1;
                current.push(ch);
            }
            ',' if brace_depth == 0 => {
                let item = current.trim();
                if !item.is_empty() {
                    parts.push(item.to_string());
                }
                current.clear();
            }
            _ => current.push(ch),
        }
    }

    let tail = current.trim();
    if !tail.is_empty() {
        parts.push(tail.to_string());
    }

    parts
}

fn parse_single_use_entry(entry: &str, aliases: &mut HashMap<String, String>) {
    let normalized = entry.trim().trim_start_matches('\\');
    if normalized.is_empty() {
        return;
    }

    let mut fqn = normalized.to_string();
    let mut alias: Option<String> = None;

    if let Some(as_pos) = normalized.to_lowercase().find(" as ") {
        let left = normalized[..as_pos].trim();
        let right = normalized[(as_pos + 4)..].trim();
        if !left.is_empty() && !right.is_empty() {
            fqn = left.to_string();
            alias = Some(right.to_string());
        }
    }

    let alias_name = alias.unwrap_or_else(|| {
        fqn.rsplit('\\')
            .next()
            .map(ToString::to_string)
            .unwrap_or_else(|| fqn.clone())
    });

    aliases.insert(alias_name, fqn);
}

fn first_namespace_in_text(text: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim_start();
        match parse_namespace_declaration(trimmed) {
            Some(NamespaceDeclaration::Inline(ns)) | Some(NamespaceDeclaration::Block(ns)) => {
                return Some(ns)
            }
            Some(NamespaceDeclaration::Global) => return None,
            None => {}
        }
    }
    None
}

fn resolve_symbol_queries(text: &str, _position: Position, name: &str) -> Vec<String> {
    let mut queries = Vec::new();
    let normalized = name.trim_start_matches('\\');

    if normalized.contains('\\') {
        queries.push(normalized.to_string());
        return queries;
    }

    let use_aliases = parse_use_aliases(text);
    if let Some(mapped) = use_aliases.get(normalized) {
        queries.push(mapped.clone());
    }

    if let Some(namespace) = first_namespace_in_text(text) {
        queries.push(format!("{}\\{}", namespace, normalized));
    }

    queries.push(normalized.to_string());
    dedup_preserve_order(queries)
}

fn dedup_preserve_order(items: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for item in items {
        if seen.insert(item.clone()) {
            out.push(item);
        }
    }
    out
}

fn symbol_matches_query(symbol: &PhpSymbol, query: &str) -> bool {
    let normalized = query.trim_start_matches('\\');
    if normalized.contains('\\') {
        return symbol.fqn() == normalized;
    }
    symbol.name == normalized
}

fn is_type_symbol_kind(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::CLASS | SymbolKind::INTERFACE | SymbolKind::ENUM | SymbolKind::MODULE
    )
}

fn find_type_definition_in_index(
    index: &HashMap<Url, Vec<PhpSymbol>>,
    current_uri: &Url,
    queries: &[String],
) -> Option<Location> {
    for query in queries {
        if let Some(symbol) = index.get(current_uri).and_then(|symbols| {
            symbols
                .iter()
                .find(|symbol| is_type_symbol_kind(symbol.kind) && symbol_matches_query(symbol, query))
        }) {
            return Some(Location {
                uri: current_uri.clone(),
                range: symbol.range,
            });
        }

        if let Some((symbol_uri, symbol)) = index.iter().find_map(|(symbol_uri, symbols)| {
            if symbol_uri == current_uri {
                return None;
            }
            symbols
                .iter()
                .find(|symbol| is_type_symbol_kind(symbol.kind) && symbol_matches_query(symbol, query))
                .map(|symbol| (symbol_uri, symbol))
        }) {
            return Some(Location {
                uri: symbol_uri.clone(),
                range: symbol.range,
            });
        }
    }

    None
}

fn is_position_within_range(pos: Position, range: Range) -> bool {
    if pos.line < range.start.line || pos.line > range.end.line {
        return false;
    }
    if pos.line == range.start.line && pos.character < range.start.character {
        return false;
    }
    if pos.line == range.end.line && pos.character > range.end.character {
        return false;
    }
    true
}

fn find_symbol_location_for_queries<'a>(
    current_uri: &'a Url,
    index: &'a HashMap<Url, Vec<PhpSymbol>>,
    queries: &[String],
) -> Option<(&'a Url, &'a PhpSymbol)> {
    for query in queries {
        if let Some(symbol) = index
            .get(current_uri)
            .and_then(|symbols| symbols.iter().find(|symbol| symbol_matches_query(symbol, query)))
        {
            return Some((current_uri, symbol));
        }

        if let Some((symbol_uri, symbol)) = index.iter().find_map(|(symbol_uri, symbols)| {
            if symbol_uri == current_uri {
                return None;
            }
            symbols
                .iter()
                .find(|symbol| symbol_matches_query(symbol, query))
                .map(|symbol| (symbol_uri, symbol))
        }) {
            return Some((symbol_uri, symbol));
        }
    }

    None
}

fn call_hierarchy_item_for_symbol(uri: &Url, symbol: &PhpSymbol) -> CallHierarchyItem {
    CallHierarchyItem {
        name: symbol.name.clone(),
        kind: symbol.kind,
        tags: None,
        detail: symbol.namespace.clone(),
        uri: uri.clone(),
        range: symbol.range,
        selection_range: symbol.range,
        data: Some(json!(symbol.fqn())),
    }
}

fn call_ranges_for_name(text: &str, name: &str) -> Vec<Range> {
    let mut ranges = Vec::new();
    for range in find_identifier_ranges(text, name) {
        let line = text
            .split('\n')
            .nth(range.end.line as usize)
            .unwrap_or_default()
            .chars()
            .collect::<Vec<_>>();
        let mut idx = range.end.character as usize;
        while idx < line.len() && line[idx].is_whitespace() {
            idx += 1;
        }
        if idx < line.len() && line[idx] == '(' {
            ranges.push(range);
        }
    }
    ranges
}

fn enclosing_function_symbol<'a>(symbols: &'a [PhpSymbol], range: Range) -> Option<&'a PhpSymbol> {
    symbols
        .iter()
        .filter(|symbol| {
            symbol.kind == SymbolKind::FUNCTION
                && is_position_within_range(range.start, symbol.range)
                && is_position_within_range(range.end, symbol.range)
        })
        .min_by(|a, b| {
            let a_size = (a.range.end.line.saturating_sub(a.range.start.line), a.range.end.character);
            let b_size = (b.range.end.line.saturating_sub(b.range.start.line), b.range.end.character);
            a_size.cmp(&b_size)
        })
}

fn resolve_completion_item(
    mut item: CompletionItem,
    index: &HashMap<Url, Vec<PhpSymbol>>,
) -> CompletionItem {
    if let Some(serde_json::Value::Object(obj)) = item.data.as_ref() {
        let member_of = obj.get("memberOf").and_then(|v| v.as_str());
        let member = obj.get("member").and_then(|v| v.as_str());
        let member_kind = obj.get("memberKind").and_then(|v| v.as_str());
        let member_type = obj.get("memberType").and_then(|v| v.as_str());
        if let (Some(member_of), Some(member), Some(member_kind)) = (member_of, member, member_kind) {
            item.detail = Some(match member_type {
                Some(type_name) => format!("{} member: {}", member_kind, type_name),
                None => format!("{} member", member_kind),
            });
            item.documentation = Some(tower_lsp::lsp_types::Documentation::MarkupContent(
                tower_lsp::lsp_types::MarkupContent {
                    kind: tower_lsp::lsp_types::MarkupKind::Markdown,
                    value: match member_type {
                        Some(type_name) => format!(
                            "**{}** `{}`\n\n**Type:** `{}`\n\n**Declared in:** `{}`",
                            if member_kind == "method" { "Method" } else { "Property" },
                            member,
                            type_name,
                            member_of
                        ),
                        None => format!(
                            "**{}** `{}`\n\n**Declared in:** `{}`",
                            if member_kind == "method" { "Method" } else { "Property" },
                            member,
                            member_of
                        ),
                    },
                },
            ));
            return item;
        }
    }

    if let Some(serde_json::Value::String(fqn)) = item.data.as_ref() {
        if let Some(symbol) = index
            .values()
            .flat_map(|symbols| symbols.iter())
            .find(|symbol| symbol.fqn() == *fqn)
        {
            item.detail = Some(match symbol.kind {
                SymbolKind::CLASS => "Class symbol".to_string(),
                SymbolKind::INTERFACE => "Interface symbol".to_string(),
                SymbolKind::ENUM => "Enum symbol".to_string(),
                SymbolKind::MODULE => "Namespace symbol".to_string(),
                SymbolKind::FUNCTION => "Function symbol".to_string(),
                SymbolKind::CONSTANT => "Constant symbol".to_string(),
                _ => "Workspace symbol".to_string(),
            });
            item.documentation = Some(tower_lsp::lsp_types::Documentation::MarkupContent(
                tower_lsp::lsp_types::MarkupContent {
                    kind: tower_lsp::lsp_types::MarkupKind::Markdown,
                    value: format_symbol_for_hover(symbol),
                },
            ));
            return item;
        }
    }

    if let Some(symbol) = index.values().flat_map(|symbols| symbols.iter()).find(|symbol| {
        let fqn = symbol.fqn();
        symbol.name == item.label || fqn == item.label || format!("\\{}", fqn) == item.label
    }) {
        item.detail = Some(match symbol.kind {
            SymbolKind::CLASS => "Class symbol".to_string(),
            SymbolKind::INTERFACE => "Interface symbol".to_string(),
            SymbolKind::ENUM => "Enum symbol".to_string(),
            SymbolKind::MODULE => "Namespace symbol".to_string(),
            SymbolKind::FUNCTION => "Function symbol".to_string(),
            SymbolKind::CONSTANT => "Constant symbol".to_string(),
            _ => "Workspace symbol".to_string(),
        });
        item.documentation = Some(tower_lsp::lsp_types::Documentation::MarkupContent(
            tower_lsp::lsp_types::MarkupContent {
                kind: tower_lsp::lsp_types::MarkupKind::Markdown,
                value: format_symbol_for_hover(symbol),
            },
        ));
        return item;
    }

    if php_magic_constants().into_iter().any(|constant| constant == item.label) {
        item.detail = Some("PHP magic constant".to_string());
        item.documentation = Some(tower_lsp::lsp_types::Documentation::String(
            "Built-in PHP magic constant.".to_string(),
        ));
    }

    item
}

fn split_fqn(fqn: &str) -> (Option<String>, String) {
    let normalized = fqn.trim_start_matches('\\');
    if let Some((namespace, short)) = normalized.rsplit_once('\\') {
        return (Some(namespace.to_string()), short.to_string());
    }
    (None, normalized.to_string())
}

fn search_terms_for_target_in_document(
    target_fqn: &str,
    doc_text: &str,
    include_alias_terms: bool,
) -> Vec<String> {
    let mut terms = HashSet::new();
    let normalized_fqn = target_fqn.trim_start_matches('\\');
    let (target_ns, target_short) = split_fqn(normalized_fqn);

    terms.insert(normalized_fqn.to_string());
    terms.insert(format!("\\{}", normalized_fqn));

    let aliases = parse_use_aliases(doc_text);
    for (alias, fqn) in aliases {
        if fqn == normalized_fqn {
            if include_alias_terms || alias == target_short {
                terms.insert(alias);
            }
            terms.insert(fqn.clone());
            terms.insert(format!("\\{}", fqn));
        }
    }

    if first_namespace_in_text(doc_text) == target_ns {
        terms.insert(target_short);
    }

    let mut out = terms.into_iter().collect::<Vec<_>>();
    out.sort();
    out
}

fn reference_locations_for_symbol(
    symbol: &PhpSymbol,
    symbol_uri: &Url,
    docs: &HashMap<Url, String>,
) -> Vec<Location> {
    let target_fqn = symbol.fqn();
    let mut locations = Vec::new();

    for (doc_uri, text) in docs {
        let terms = search_terms_for_target_in_document(&target_fqn, text, true);
        for term in terms {
            for range in find_identifier_ranges(text, &term) {
                if *doc_uri == *symbol_uri
                    && range.start.line == symbol.range.start.line
                    && range.start.character <= symbol.range.start.character
                    && range.end.character >= symbol.range.start.character.saturating_add(1)
                {
                    continue;
                }
                locations.push(Location {
                    uri: doc_uri.clone(),
                    range,
                });
            }
        }
    }

    locations.sort_by(|a, b| {
        a.uri
            .as_str()
            .cmp(b.uri.as_str())
            .then_with(|| a.range.start.line.cmp(&b.range.start.line))
            .then_with(|| a.range.start.character.cmp(&b.range.start.character))
    });
    locations.dedup_by(|a, b| {
        a.uri == b.uri && a.range.start == b.range.start && a.range.end == b.range.end
    });

    locations
}

fn reference_count_title(count: usize) -> String {
    match count {
        0 => "No references".to_string(),
        1 => "1 reference".to_string(),
        _ => format!("{count} references"),
    }
}

fn build_function_parameter_map(
    symbols: &HashMap<Url, Vec<PhpSymbol>>,
) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();

    for symbols_in_doc in symbols.values() {
        for symbol in symbols_in_doc {
            if symbol.kind != SymbolKind::FUNCTION {
                continue;
            }

            map.entry(symbol.name.clone())
                .or_insert_with(|| symbol.parameters.clone());
        }
    }

    map
}

fn collect_parameter_inlay_hints_for_range(
    text: &str,
    start_line: u32,
    end_line: u32,
    function_param_map: &HashMap<String, Vec<String>>,
) -> Vec<(u32, u32, String)> {
    let mut result = Vec::new();
    let mut in_block_comment = false;

    for (line_idx, line) in text.lines().enumerate() {
        let line_u32 = line_idx as u32;
        let chars: Vec<char> = line.chars().collect();
        let mask = code_mask_for_line(&chars, &mut in_block_comment);

        if line_u32 < start_line || line_u32 > end_line {
            continue;
        }

        result.extend(collect_parameter_hints_for_line(
            &chars,
            &mask,
            line_u32,
            function_param_map,
        ));
    }

    result
}

fn collect_parameter_hints_for_line(
    chars: &[char],
    code_mask: &[bool],
    line: u32,
    function_param_map: &HashMap<String, Vec<String>>,
) -> Vec<(u32, u32, String)> {
    let mut hints = Vec::new();
    let mut i = 0usize;

    while i < chars.len() {
        if chars[i] != '(' || !code_mask.get(i).copied().unwrap_or(false) {
            i += 1;
            continue;
        }

        let mut name_end = i;
        while name_end > 0 && chars[name_end - 1].is_whitespace() {
            name_end -= 1;
        }

        let mut name_start = name_end;
        while name_start > 0 && (chars[name_start - 1].is_ascii_alphanumeric() || chars[name_start - 1] == '_' || chars[name_start - 1] == '\\') {
            name_start -= 1;
        }

        if name_start == name_end {
            i += 1;
            continue;
        }

        let function_name: String = chars[name_start..name_end].iter().collect();
        if function_name.is_empty() {
            i += 1;
            continue;
        }

        // Skip function declarations.
        let prefix = chars[..name_start].iter().collect::<String>();
        if prefix.trim_end().ends_with("function") {
            i += 1;
            continue;
        }

        let short_name = function_name.rsplit('\\').next().unwrap_or(&function_name);
        let Some(parameters) = function_param_map.get(short_name) else {
            i += 1;
            continue;
        };
        if parameters.is_empty() {
            i += 1;
            continue;
        }

        let mut j = i + 1;
        let mut nested_paren_depth = 0i32;
        let mut nested_bracket_depth = 0i32;
        let mut nested_brace_depth = 0i32;
        let mut arg_index = 0usize;
        let mut arg_start = first_non_whitespace(chars, code_mask, j);

        while j < chars.len() {
            if !code_mask.get(j).copied().unwrap_or(false) {
                j += 1;
                continue;
            }

            match chars[j] {
                '(' => {
                    nested_paren_depth += 1;
                }
                ')' => {
                    if nested_paren_depth == 0
                        && nested_bracket_depth == 0
                        && nested_brace_depth == 0
                    {
                        if let Some(start) = arg_start {
                            if arg_index < parameters.len() && arg_has_content(chars, code_mask, start, j) {
                                hints.push((
                                    line,
                                    start as u32,
                                    format!("{}:", parameter_hint_name(&parameters[arg_index], arg_index)),
                                ));
                            }
                        }
                        break;
                    }
                    nested_paren_depth -= 1;
                }
                '[' => nested_bracket_depth += 1,
                ']' => {
                    if nested_bracket_depth > 0 {
                        nested_bracket_depth -= 1;
                    }
                }
                '{' => nested_brace_depth += 1,
                '}' => {
                    if nested_brace_depth > 0 {
                        nested_brace_depth -= 1;
                    }
                }
                ',' => {
                    if nested_paren_depth == 0
                        && nested_bracket_depth == 0
                        && nested_brace_depth == 0
                    {
                        if let Some(start) = arg_start {
                            if arg_index < parameters.len() && arg_has_content(chars, code_mask, start, j) {
                                hints.push((
                                    line,
                                    start as u32,
                                    format!("{}:", parameter_hint_name(&parameters[arg_index], arg_index)),
                                ));
                            }
                        }

                        arg_index += 1;
                        arg_start = first_non_whitespace(chars, code_mask, j + 1);
                    }
                }
                _ => {}
            }

            j += 1;
        }

        i = j.saturating_add(1);
    }

    hints
}

fn collect_return_type_inlay_hints_for_range(
    symbols: &HashMap<Url, Vec<PhpSymbol>>,
    uri: &Url,
    start_line: u32,
    end_line: u32,
) -> Vec<InlayHint> {
    let mut hints = Vec::new();
    let Some(symbols_in_doc) = symbols.get(uri) else {
        return hints;
    };

    for symbol in symbols_in_doc {
        if symbol.kind != SymbolKind::FUNCTION {
            continue;
        }
        let Some(return_type) = &symbol.return_type else {
            continue;
        };
        let line = symbol.range.end.line;
        if line < start_line || line > end_line {
            continue;
        }

        hints.push(InlayHint {
            position: symbol.range.end,
            label: InlayHintLabel::String(format!(": {return_type}")),
            kind: Some(InlayHintKind::TYPE),
            text_edits: None,
            tooltip: None,
            padding_left: Some(true),
            padding_right: Some(false),
            data: None,
        });
    }

    hints
}

fn first_non_whitespace(chars: &[char], code_mask: &[bool], mut start: usize) -> Option<usize> {
    while start < chars.len() {
        if !code_mask.get(start).copied().unwrap_or(false) {
            start += 1;
            continue;
        }
        if !chars[start].is_whitespace() {
            return Some(start);
        }
        start += 1;
    }
    None
}

fn arg_has_content(chars: &[char], code_mask: &[bool], start: usize, end_exclusive: usize) -> bool {
    let mut idx = start;
    while idx < end_exclusive && idx < chars.len() {
        if code_mask.get(idx).copied().unwrap_or(false)
            && (!chars[idx].is_whitespace() || chars[idx] == '\'' || chars[idx] == '"')
        {
            return true;
        }
        idx += 1;
    }
    false
}

fn parameter_hint_name(parameter: &str, arg_index: usize) -> String {
    if let Some(name) = extract_first_variable_name(parameter) {
        return name.trim_start_matches('$').to_string();
    }
    format!("arg{}", arg_index + 1)
}

fn collect_class_implementation_locations(
    target_fqn: &str,
    docs: &HashMap<Url, String>,
    symbols: &HashMap<Url, Vec<PhpSymbol>>,
) -> Vec<Location> {
    let target_fqn = target_fqn.trim_start_matches('\\');
    let (_, target_short) = split_fqn(target_fqn);
    let mut result = Vec::new();

    for (doc_uri, text) in docs {
        let aliases = parse_use_aliases(text);
        let namespace = first_namespace_in_text(text);
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
            if !trimmed.contains("class ") {
                continue;
            }

            let Some(class_name) = token_after_keyword(trimmed, "class") else {
                continue;
            };
            let (extends_target, implements_targets) = extract_class_relationship_targets(trimmed);

            let mut matches_target = false;
            if let Some(ext) = extends_target {
                if type_reference_matches_target(
                    &ext,
                    target_fqn,
                    &target_short,
                    namespace.as_deref(),
                    &aliases,
                ) {
                    matches_target = true;
                }
            }
            if !matches_target {
                for item in implements_targets {
                    if type_reference_matches_target(
                        &item,
                        target_fqn,
                        &target_short,
                        namespace.as_deref(),
                        &aliases,
                    ) {
                        matches_target = true;
                        break;
                    }
                }
            }
            if !matches_target {
                continue;
            }

            if let Some(symbols_in_doc) = symbols.get(doc_uri) {
                if let Some(class_symbol) = symbols_in_doc.iter().find(|sym| {
                    sym.kind == SymbolKind::CLASS
                        && sym.name == class_name
                        && (sym.range.start.line as i32 - line_idx as i32).abs() <= 1
                }) {
                    result.push(Location {
                        uri: doc_uri.clone(),
                        range: class_symbol.range,
                    });
                    continue;
                }
                if let Some(class_symbol) = symbols_in_doc
                    .iter()
                    .find(|sym| sym.kind == SymbolKind::CLASS && sym.name == class_name)
                {
                    result.push(Location {
                        uri: doc_uri.clone(),
                        range: class_symbol.range,
                    });
                }
            }
        }
    }

    result.sort_by(|a, b| {
        a.uri
            .as_str()
            .cmp(b.uri.as_str())
            .then_with(|| a.range.start.line.cmp(&b.range.start.line))
            .then_with(|| a.range.start.character.cmp(&b.range.start.character))
    });
    result.dedup_by(|a, b| {
        a.uri == b.uri && a.range.start == b.range.start && a.range.end == b.range.end
    });
    result
}

fn extract_class_relationship_targets(line: &str) -> (Option<String>, Vec<String>) {
    let mut extends_target = None;
    let mut implements_targets = Vec::new();

    let lower = line.to_lowercase();
    if let Some(extends_idx) = lower.find(" extends ") {
        let after_extends = &line[(extends_idx + 9)..];
        let before_impl = after_extends
            .split(" implements ")
            .next()
            .unwrap_or(after_extends);
        let token = parse_type_token(before_impl);
        if !token.is_empty() {
            extends_target = Some(token);
        }
    }

    if let Some(implements_idx) = lower.find(" implements ") {
        let after_impl = &line[(implements_idx + 12)..];
        let before_block = after_impl.split('{').next().unwrap_or(after_impl);
        for part in before_block.split(',') {
            let token = parse_type_token(part);
            if !token.is_empty() {
                implements_targets.push(token);
            }
        }
    }

    (extends_target, implements_targets)
}

fn parse_type_token(input: &str) -> String {
    input
        .trim()
        .trim_start_matches('&')
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '\\')
        .collect::<String>()
}

fn resolve_type_reference_to_fqn(
    reference: &str,
    namespace: Option<&str>,
    aliases: &HashMap<String, String>,
) -> String {
    let normalized = reference.trim().trim_start_matches('\\');
    if normalized.contains('\\') {
        return normalized.to_string();
    }
    if let Some(mapped) = aliases.get(normalized) {
        return mapped.clone();
    }
    if let Some(ns) = namespace {
        if !ns.is_empty() {
            return format!("{ns}\\{normalized}");
        }
    }
    normalized.to_string()
}

fn type_reference_matches_target(
    reference: &str,
    target_fqn: &str,
    target_short: &str,
    namespace: Option<&str>,
    aliases: &HashMap<String, String>,
) -> bool {
    let resolved = resolve_type_reference_to_fqn(reference, namespace, aliases);
    let (_, resolved_short) = split_fqn(&resolved);
    resolved == target_fqn || resolved == target_short || resolved_short == target_short
}

fn collect_folding_ranges(text: &str) -> Vec<FoldingRange> {
    let lines: Vec<&str> = text.lines().collect();
    let mut ranges = Vec::new();
    let mut stack: Vec<u32> = Vec::new();
    let mut in_block_comment = false;

    for (line_idx, line) in lines.iter().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let mask = code_mask_for_line(&chars, &mut in_block_comment);

        for (idx, ch) in chars.iter().enumerate() {
            if !mask.get(idx).copied().unwrap_or(false) {
                continue;
            }

            if *ch == '{' {
                stack.push(line_idx as u32);
            } else if *ch == '}' {
                if let Some(start_line) = stack.pop() {
                    if (line_idx as u32) > start_line {
                        ranges.push(FoldingRange {
                            start_line,
                            start_character: None,
                            end_line: line_idx as u32,
                            end_character: None,
                            kind: None,
                            collapsed_text: None,
                        });
                    }
                }
            }
        }
    }

    // Fold contiguous import blocks (use statements).
    let mut first_use: Option<u32> = None;
    let mut prev_use: Option<u32> = None;
    for (line_idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("use ") {
            if first_use.is_none() {
                first_use = Some(line_idx as u32);
            }
            prev_use = Some(line_idx as u32);
            continue;
        }

        if let (Some(start), Some(end)) = (first_use, prev_use) {
            if end > start {
                ranges.push(FoldingRange {
                    start_line: start,
                    start_character: None,
                    end_line: end,
                    end_character: None,
                    kind: Some(tower_lsp::lsp_types::FoldingRangeKind::Imports),
                    collapsed_text: None,
                });
            }
        }
        first_use = None;
        prev_use = None;
    }
    if let (Some(start), Some(end)) = (first_use, prev_use) {
        if end > start {
            ranges.push(FoldingRange {
                start_line: start,
                start_character: None,
                end_line: end,
                end_character: None,
                kind: Some(tower_lsp::lsp_types::FoldingRangeKind::Imports),
                collapsed_text: None,
            });
        }
    }

    ranges.sort_by(|a, b| {
        a.start_line
            .cmp(&b.start_line)
            .then_with(|| a.end_line.cmp(&b.end_line))
    });
    ranges.dedup_by(|a, b| {
        a.start_line == b.start_line && a.end_line == b.end_line && a.kind == b.kind
    });
    ranges
}

fn looks_like_type_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_uppercase() || first == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn detect_http_urls(text: &str) -> Vec<DocumentLink> {
    let mut links = Vec::new();
    let trailing_punctuation = ['.', ',', ';', ':', '!', '?'];

    for (line_idx, line) in text.lines().enumerate() {
        let bytes = line.as_bytes();
        let mut i = 0usize;

        while i < bytes.len() {
            let is_http = bytes[i..].starts_with(b"http://") || bytes[i..].starts_with(b"https://");
            if !is_http {
                i += 1;
                continue;
            }

            let start = i;
            let mut end = i;
            while end < bytes.len() {
                let ch = bytes[end] as char;
                if ch.is_whitespace() || matches!(ch, '"' | '\'' | ')' | ']' | '}') {
                    break;
                }
                end += 1;
            }

            let mut trimmed_end = end;
            while trimmed_end > start {
                let ch = line.as_bytes()[trimmed_end - 1] as char;
                if trailing_punctuation.contains(&ch) {
                    trimmed_end -= 1;
                } else {
                    break;
                }
            }

            let candidate = &line[start..trimmed_end];
            if let Ok(target) = Url::parse(candidate) {
                links.push(DocumentLink {
                    range: Range::new(
                        Position::new(line_idx as u32, start as u32),
                        Position::new(line_idx as u32, trimmed_end as u32),
                    ),
                    target: Some(target),
                    tooltip: None,
                    data: None,
                });
            }

            i = end;
        }
    }

    links
}

#[derive(Clone, Debug)]
struct ParsedSemanticToken {
    line: u32,
    char: u32,
    length: u32,
    token_type: u32,
}

fn tokenize_php_document(text: &str) -> Vec<ParsedSemanticToken> {
    let mut tokens = Vec::new();

    tokens.extend(collect_namespace_tokens(text));
    tokens.extend(collect_blade_directive_semantic_tokens(text));
    tokens.extend(collect_html_tag_semantic_tokens(text));
    tokens.extend(collect_embedded_js_css_semantic_tokens(text));

    let symbols = extract_symbols(text);
    for symbol in symbols {
        if !is_code_position(text, symbol.range.start) {
            continue;
        }

        let token_type = match symbol.kind {
            SymbolKind::FUNCTION => Some(1),
            SymbolKind::CLASS => Some(2),
            SymbolKind::INTERFACE => Some(3),
            SymbolKind::ENUM => Some(4),
            _ => None,
        };

        let Some(token_type) = token_type else {
            continue;
        };

        let length = symbol
            .range
            .end
            .character
            .saturating_sub(symbol.range.start.character);
        if length == 0 {
            continue;
        }

        tokens.push(ParsedSemanticToken {
            line: symbol.range.start.line,
            char: symbol.range.start.character,
            length,
            token_type,
        });
    }

    tokens.extend(collect_variable_semantic_tokens(text));

    tokens.sort_by(|a, b| a.line.cmp(&b.line).then_with(|| a.char.cmp(&b.char)));
    tokens.dedup_by(|a, b| {
        a.line == b.line
            && a.char == b.char
            && a.length == b.length
            && a.token_type == b.token_type
    });
    tokens
}

fn collect_namespace_tokens(text: &str) -> Vec<ParsedSemanticToken> {
    let mut tokens = Vec::new();
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
        if !trimmed.starts_with("namespace ") {
            continue;
        }

        let rest = trimmed.trim_start_matches("namespace ").trim_start();
        let namespace: String = rest
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '\\')
            .collect();
        if namespace.is_empty() {
            continue;
        }

        if let Some(column) = sanitized.find(&namespace) {
            tokens.push(ParsedSemanticToken {
                line: line_idx as u32,
                char: column as u32,
                length: namespace.len() as u32,
                token_type: 5,
            });
        }
    }

    tokens
}

fn collect_blade_directive_semantic_tokens(text: &str) -> Vec<ParsedSemanticToken> {
    let directives = blade_directive_keywords();
    let mut tokens = Vec::new();

    for (line_idx, line) in text.lines().enumerate() {
        let bytes = line.as_bytes();
        for directive in directives.iter() {
            let mut rest = line;
            let mut offset = 0usize;
            while let Some(found) = rest.find(directive) {
                let start = offset + found;
                let end = start + directive.len();

                let left_ok = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
                let right_ok = end >= bytes.len() || !bytes[end].is_ascii_alphanumeric();

                if left_ok && right_ok {
                    tokens.push(ParsedSemanticToken {
                        line: line_idx as u32,
                        char: start as u32,
                        length: directive.len() as u32,
                        token_type: 1,
                    });
                }

                let advance = found + directive.len();
                rest = &rest[advance..];
                offset += advance;
            }
        }
    }

    tokens
}

fn collect_html_tag_semantic_tokens(text: &str) -> Vec<ParsedSemanticToken> {
    let mut tokens = Vec::new();
    for (line_idx, line) in text.lines().enumerate() {
        let bytes = line.as_bytes();
        let mut i = 0usize;
        while i < bytes.len() {
            if bytes[i] != b'<' {
                i += 1;
                continue;
            }

            let mut j = i + 1;
            if j < bytes.len() && bytes[j] == b'/' {
                j += 1;
            }
            let start = j;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'-') {
                j += 1;
            }
            if j > start {
                tokens.push(ParsedSemanticToken {
                    line: line_idx as u32,
                    char: start as u32,
                    length: (j - start) as u32,
                    token_type: 5,
                });
            }
            i = j.saturating_add(1);
        }
    }
    tokens
}

fn collect_embedded_js_css_semantic_tokens(text: &str) -> Vec<ParsedSemanticToken> {
    let mut tokens = Vec::new();
    let mut in_script = false;
    let mut in_style = false;

    for (line_idx, line) in text.lines().enumerate() {
        let lower = line.to_ascii_lowercase();
        if lower.contains("<script") {
            in_script = true;
        }
        if lower.contains("</script") {
            in_script = false;
        }
        if lower.contains("<style") {
            in_style = true;
        }
        if lower.contains("</style") {
            in_style = false;
        }

        if in_script {
            tokens.extend(collect_word_tokens_for_line(
                line,
                line_idx as u32,
                &["function", "const", "let", "var", "class", "return", "if", "else", "for", "while"],
                1,
            ));
        }

        if in_style {
            let bytes = line.as_bytes();
            let mut i = 0usize;
            while i < bytes.len() {
                while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-') {
                    i += 1;
                }
                if i > start {
                    let mut j = i;
                    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    if j < bytes.len() && bytes[j] == b':' {
                        tokens.push(ParsedSemanticToken {
                            line: line_idx as u32,
                            char: start as u32,
                            length: (i - start) as u32,
                            token_type: 0,
                        });
                    }
                }
                i = i.saturating_add(1);
            }
        }
    }

    tokens
}

fn collect_word_tokens_for_line(
    line: &str,
    line_idx: u32,
    words: &[&str],
    token_type: u32,
) -> Vec<ParsedSemanticToken> {
    let mut tokens = Vec::new();
    let bytes = line.as_bytes();

    for word in words {
        let mut rest = line;
        let mut offset = 0usize;
        while let Some(found) = rest.find(word) {
            let start = offset + found;
            let end = start + word.len();
            let left_ok = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
            let right_ok = end >= bytes.len() || !bytes[end].is_ascii_alphanumeric();
            if left_ok && right_ok {
                tokens.push(ParsedSemanticToken {
                    line: line_idx,
                    char: start as u32,
                    length: word.len() as u32,
                    token_type,
                });
            }
            let advance = found + word.len();
            rest = &rest[advance..];
            offset += advance;
        }
    }

    tokens
}

fn collect_variable_semantic_tokens(text: &str) -> Vec<ParsedSemanticToken> {
    let mut tokens = Vec::new();
    let mut in_block_comment = false;

    for (line_idx, line) in text.lines().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let mask = code_mask_for_line(&chars, &mut in_block_comment);
        for (name, start, end_exclusive) in variable_occurrences_in_line(&chars, &mask) {
            if is_builtin_variable(&name) {
                continue;
            }

            tokens.push(ParsedSemanticToken {
                line: line_idx as u32,
                char: start as u32,
                length: (end_exclusive - start) as u32,
                token_type: 0,
            });
        }
    }

    tokens
}

fn compress_tokens_to_semantic_data(tokens: Vec<ParsedSemanticToken>) -> Vec<SemanticToken> {
    let mut data = Vec::new();
    let mut prev_line = 0u32;
    let mut prev_char = 0u32;

    for token in tokens {
        let line_delta = token.line.saturating_sub(prev_line);
        let char_delta = if token.line == prev_line {
            token.char.saturating_sub(prev_char)
        } else {
            token.char
        };

        data.push(SemanticToken {
            delta_line: line_delta,
            delta_start: char_delta,
            length: token.length,
            token_type: token.token_type,
            token_modifiers_bitset: 0,
        });

        prev_line = token.line;
        prev_char = token.char;
    }

    data
}

fn find_use_insertion_position(text: &str) -> Position {
    let mut insert_line = 0u32;
    let mut last_use_or_namespace: Option<u32> = None;

    for (idx, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if idx == 0 && trimmed.starts_with("<?php") {
            insert_line = 1;
        }
        if trimmed.starts_with("namespace ") || trimmed.starts_with("use ") {
            last_use_or_namespace = Some(idx as u32);
        }
    }

    if let Some(line) = last_use_or_namespace {
        Position::new(line + 1, 0)
    } else {
        Position::new(insert_line, 0)
    }
}

fn import_action_for_fqn(
    fqn: &str,
    existing_imports: &HashMap<String, String>,
) -> Option<(String, String)> {
    let (_, short_name) = split_fqn(fqn);
    if short_name.is_empty() {
        return None;
    }

    // Defensive guard for direct helper use, even though code_action pre-filters this.
    if existing_imports.values().any(|existing| existing == fqn) {
        return None;
    }

    if let Some(conflict_fqn) = existing_imports.get(&short_name) {
        if conflict_fqn == fqn {
            return None;
        }

        let alias = unique_import_alias(&short_name, existing_imports);
        return Some((
            format!("use {} as {};\n", fqn, alias),
            format!("Add use {} as {}", fqn, alias),
        ));
    }

    Some((format!("use {};\n", fqn), format!("Add use {}", fqn)))
}

fn var_dump_delete_action(
    diagnostic: &Diagnostic,
    text: &str,
    uri: &Url,
) -> Option<CodeActionOrCommand> {
    if !diagnostic
        .message
        .contains("Avoid leaving debug output in committed code.")
    {
        return None;
    }

    let line = diagnostic.range.start.line as usize;
    let (start, end) = delete_line_range(text, line)?;

    let edit = WorkspaceEdit {
        changes: Some(HashMap::from([(
            uri.clone(),
            vec![TextEdit {
                range: Range::new(start, end),
                new_text: String::new(),
            }],
        )])),
        document_changes: None,
        change_annotations: None,
    };

    Some(CodeActionOrCommand::CodeAction(CodeAction {
        title: "Remove debug var_dump".to_string(),
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: Some(vec![diagnostic.clone()]),
        edit: Some(edit),
        command: None,
        is_preferred: Some(false),
        disabled: None,
        data: None,
    }))
}

fn php_tag_insert_action(
    diagnostic: &Diagnostic,
    text: &str,
    uri: &Url,
) -> Option<CodeActionOrCommand> {
    if !diagnostic
        .message
        .contains("PHP file should contain an opening '<?php' tag.")
    {
        return None;
    }

    if text.trim_start().starts_with("<?php") {
        return None;
    }

    let edit = WorkspaceEdit {
        changes: Some(HashMap::from([(
            uri.clone(),
            vec![TextEdit {
                range: Range::new(Position::new(0, 0), Position::new(0, 0)),
                new_text: "<?php\n".to_string(),
            }],
        )])),
        document_changes: None,
        change_annotations: None,
    };

    Some(CodeActionOrCommand::CodeAction(CodeAction {
        title: "Add opening '<?php' tag".to_string(),
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: Some(vec![diagnostic.clone()]),
        edit: Some(edit),
        command: None,
        is_preferred: Some(true),
        disabled: None,
        data: None,
    }))
}

fn undefined_var_declare_action(
    diagnostic: &Diagnostic,
    text: &str,
    uri: &Url,
) -> Option<CodeActionOrCommand> {
    let var_name = diagnostic.message.strip_prefix("Undefined variable: ")?;
    if var_name.is_empty() || !var_name.starts_with('$') || is_builtin_variable(var_name) {
        return None;
    }

    let line = diagnostic.range.start.line as usize;
    let line_text = text.split('\n').nth(line)?;
    let indent: String = line_text
        .chars()
        .take_while(|ch| ch.is_whitespace())
        .collect();

    // Keep this quick fix simple and predictable: insert declaration directly above
    // the diagnostic line without attempting scope reconstruction.
    let insert = Position::new(line as u32, 0);
    let edit = WorkspaceEdit {
        changes: Some(HashMap::from([(
            uri.clone(),
            vec![TextEdit {
                range: Range::new(insert, insert),
                new_text: format!("{indent}{var_name} = null;\n"),
            }],
        )])),
        document_changes: None,
        change_annotations: None,
    };

    Some(CodeActionOrCommand::CodeAction(CodeAction {
        title: format!("Declare {var_name} = null"),
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: Some(vec![diagnostic.clone()]),
        edit: Some(edit),
        command: None,
        is_preferred: Some(false),
        disabled: None,
        data: None,
    }))
}

fn unused_import_remove_action(
    diagnostic: &Diagnostic,
    text: &str,
    uri: &Url,
) -> Option<CodeActionOrCommand> {
    let import_name = diagnostic.message.strip_prefix("Unused import: ")?;
    if import_name.trim().is_empty() {
        return None;
    }

    let line = diagnostic.range.start.line as usize;
    let (start, end) = delete_line_range(text, line)?;

    let edit = WorkspaceEdit {
        changes: Some(HashMap::from([(
            uri.clone(),
            vec![TextEdit {
                range: Range::new(start, end),
                new_text: String::new(),
            }],
        )])),
        document_changes: None,
        change_annotations: None,
    };

    Some(CodeActionOrCommand::CodeAction(CodeAction {
        title: format!("Remove unused import {import_name}"),
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: Some(vec![diagnostic.clone()]),
        edit: Some(edit),
        command: None,
        is_preferred: Some(false),
        disabled: None,
        data: None,
    }))
}

fn duplicate_import_remove_action(
    diagnostic: &Diagnostic,
    text: &str,
    uri: &Url,
) -> Option<CodeActionOrCommand> {
    let import_name = diagnostic.message.strip_prefix("Duplicate import: ")?;
    if import_name.trim().is_empty() {
        return None;
    }

    let line = diagnostic.range.start.line as usize;
    let (start, end) = delete_line_range(text, line)?;
    let edit = WorkspaceEdit {
        changes: Some(HashMap::from([(
            uri.clone(),
            vec![TextEdit {
                range: Range::new(start, end),
                new_text: String::new(),
            }],
        )])),
        document_changes: None,
        change_annotations: None,
    };

    Some(CodeActionOrCommand::CodeAction(CodeAction {
        title: format!("Remove duplicate import {import_name}"),
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: Some(vec![diagnostic.clone()]),
        edit: Some(edit),
        command: None,
        is_preferred: Some(false),
        disabled: None,
        data: None,
    }))
}

fn operator_confusion_compare_action(
    diagnostic: &Diagnostic,
    uri: &Url,
) -> Option<CodeActionOrCommand> {
    if diagnostic.message != "Suspicious assignment '=' in conditional expression" {
        return None;
    }

    let edit = WorkspaceEdit {
        changes: Some(HashMap::from([(
            uri.clone(),
            vec![TextEdit {
                range: diagnostic.range,
                new_text: "==".to_string(),
            }],
        )])),
        document_changes: None,
        change_annotations: None,
    };

    Some(CodeActionOrCommand::CodeAction(CodeAction {
        title: "Replace '=' with '=='".to_string(),
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: Some(vec![diagnostic.clone()]),
        edit: Some(edit),
        command: None,
        is_preferred: Some(true),
        disabled: None,
        data: None,
    }))
}

fn unused_variable_remove_action(
    diagnostic: &Diagnostic,
    text: &str,
    uri: &Url,
) -> Option<CodeActionOrCommand> {
    let var_name = diagnostic.message.strip_prefix("Unused variable: ")?;
    if var_name.trim().is_empty() {
        return None;
    }

    let line = diagnostic.range.start.line as usize;
    let (start, end) = delete_line_range(text, line)?;
    let edit = WorkspaceEdit {
        changes: Some(HashMap::from([(
            uri.clone(),
            vec![TextEdit {
                range: Range::new(start, end),
                new_text: String::new(),
            }],
        )])),
        document_changes: None,
        change_annotations: None,
    };

    Some(CodeActionOrCommand::CodeAction(CodeAction {
        title: format!("Remove unused variable {var_name}"),
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: Some(vec![diagnostic.clone()]),
        edit: Some(edit),
        command: None,
        is_preferred: Some(false),
        disabled: None,
        data: None,
    }))
}

fn brace_mismatch_fix_action(
    diagnostic: &Diagnostic,
    text: &str,
    uri: &Url,
) -> Option<CodeActionOrCommand> {
    if diagnostic.message == "Unexpected closing brace '}'" {
        let edit = WorkspaceEdit {
            changes: Some(HashMap::from([(
                uri.clone(),
                vec![TextEdit {
                    range: diagnostic.range,
                    new_text: String::new(),
                }],
            )])),
            document_changes: None,
            change_annotations: None,
        };

        return Some(CodeActionOrCommand::CodeAction(CodeAction {
            title: "Remove unexpected '}'".to_string(),
            kind: Some(CodeActionKind::QUICKFIX),
            diagnostics: Some(vec![diagnostic.clone()]),
            edit: Some(edit),
            command: None,
            is_preferred: Some(false),
            disabled: None,
            data: None,
        }));
    }

    if diagnostic.message == "Unclosed opening brace '{'" {
        let insert = document_end_position(text);
        let suffix = if text.ends_with('\n') { "}\n" } else { "\n}\n" };
        let edit = WorkspaceEdit {
            changes: Some(HashMap::from([(
                uri.clone(),
                vec![TextEdit {
                    range: Range::new(insert, insert),
                    new_text: suffix.to_string(),
                }],
            )])),
            document_changes: None,
            change_annotations: None,
        };

        return Some(CodeActionOrCommand::CodeAction(CodeAction {
            title: "Add missing closing '}'".to_string(),
            kind: Some(CodeActionKind::QUICKFIX),
            diagnostics: Some(vec![diagnostic.clone()]),
            edit: Some(edit),
            command: None,
            is_preferred: Some(false),
            disabled: None,
            data: None,
        }));
    }

    None
}

fn missing_return_type_add_action(
    diagnostic: &Diagnostic,
    text: &str,
    uri: &Url,
) -> Option<CodeActionOrCommand> {
    let fn_name = diagnostic.message.strip_prefix("Missing return type: ")?;
    if !fn_name.ends_with("()") {
        return None;
    }

    let line_idx = diagnostic.range.start.line as usize;
    let insert = find_return_type_insertion_point(text, line_idx)?;
    let edit = WorkspaceEdit {
        changes: Some(HashMap::from([(
            uri.clone(),
            vec![TextEdit {
                range: Range::new(insert, insert),
                new_text: ": mixed".to_string(),
            }],
        )])),
        document_changes: None,
        change_annotations: None,
    };

    Some(CodeActionOrCommand::CodeAction(CodeAction {
        title: format!("Add return type to {}", fn_name),
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: Some(vec![diagnostic.clone()]),
        edit: Some(edit),
        command: None,
        is_preferred: Some(false),
        disabled: None,
        data: None,
    }))
}

fn find_return_type_insertion_point(text: &str, start_line: usize) -> Option<Position> {
    let lines = text.split('\n').collect::<Vec<_>>();
    let start_text = lines.get(start_line)?;
    let function_col = function_keyword_column(start_text)?;

    let mut found_open = false;
    let mut paren_depth = 0i32;

    for line_idx in start_line..lines.len() {
        let chars = lines[line_idx].chars().collect::<Vec<_>>();
        let scan_start = if line_idx == start_line { function_col } else { 0 };
        for col in scan_start..chars.len() {
            let ch = chars[col];
            if !found_open {
                if ch == '(' {
                    found_open = true;
                    paren_depth = 1;
                }
                continue;
            }

            if ch == '(' {
                paren_depth += 1;
                continue;
            }
            if ch == ')' {
                paren_depth -= 1;
                if paren_depth == 0 {
                    let close_line = line_idx;
                    let close_col = col;
                    let mut probe_line = line_idx;
                    let mut probe_col = col + 1;
                    loop {
                        let probe_chars = lines[probe_line].chars().collect::<Vec<_>>();
                        while probe_col < probe_chars.len() && probe_chars[probe_col].is_whitespace() {
                            probe_col += 1;
                        }

                        if probe_col < probe_chars.len() {
                            if probe_chars[probe_col] == ':' {
                                return None;
                            }
                            return Some(Position::new(close_line as u32, (close_col + 1) as u32));
                        }

                        probe_line += 1;
                        if probe_line >= lines.len() {
                            return Some(Position::new(close_line as u32, (close_col + 1) as u32));
                        }
                        probe_col = 0;
                    }
                }
            }
        }
    }

    None
}

fn function_keyword_column(line: &str) -> Option<usize> {
    let chars = line.chars().collect::<Vec<_>>();
    let needle = "function".chars().collect::<Vec<_>>();
    if chars.len() < needle.len() {
        return None;
    }

    for idx in 0..=(chars.len() - needle.len()) {
        if chars[idx..(idx + needle.len())] != needle[..] {
            continue;
        }

        let before_ok = idx == 0 || !chars[idx - 1].is_ascii_alphanumeric();
        let after_idx = idx + needle.len();
        let after_ok = after_idx >= chars.len() || !chars[after_idx].is_ascii_alphanumeric();
        if !before_ok || !after_ok {
            continue;
        }

        let before_text = chars[..idx].iter().collect::<String>();
        let before_trimmed = before_text.trim_end();
        if before_trimmed.ends_with("//") || before_trimmed.ends_with('#') {
            continue;
        }

        return Some(idx);
    }

    None
}

fn unique_import_alias(short_name: &str, existing_imports: &HashMap<String, String>) -> String {
    let mut candidate = format!("{}Alias", short_name);
    let mut counter = 2usize;
    while existing_imports.contains_key(&candidate) {
        candidate = format!("{}Alias{}", short_name, counter);
        counter += 1;
    }
    candidate
}

fn delete_line_range(text: &str, line: usize) -> Option<(Position, Position)> {
    let lines = text.split('\n').collect::<Vec<_>>();
    if line >= lines.len() {
        return None;
    }

    let line_end = lines[line].chars().count() as u32;
    if line + 1 < lines.len() {
        return Some((
            Position::new(line as u32, 0),
            Position::new((line + 1) as u32, 0),
        ));
    }

    if line == 0 {
        return Some((Position::new(0, 0), Position::new(0, line_end)));
    }

    let prev_end = lines[line - 1].chars().count() as u32;
    Some((
        Position::new((line - 1) as u32, prev_end),
        Position::new(line as u32, line_end),
    ))
}

fn symbol_completion_label(symbol: &PhpSymbol, context: CompletionContextKind) -> String {
    if context == CompletionContextKind::UseStatement {
        return symbol.fqn();
    }
    symbol.name.clone()
}

fn format_symbol_for_hover(symbol: &PhpSymbol) -> String {
    format_symbol_for_hover_with_templates(symbol, &[])
}

fn symbol_display_name_with_templates(name: &str, template_params: &[String]) -> String {
    if template_params.is_empty() {
        return name.to_string();
    }
    format!("{}<{}>", name, template_params.join(", "))
}

fn format_symbol_for_hover_with_templates(symbol: &PhpSymbol, template_params: &[String]) -> String {
    let kind = match symbol.kind {
        SymbolKind::CLASS => "Class",
        SymbolKind::FUNCTION => "Function",
        SymbolKind::INTERFACE => "Interface",
        SymbolKind::MODULE => "Trait",
        SymbolKind::ENUM => "Enum",
        SymbolKind::CONSTANT => "Constant",
        SymbolKind::VARIABLE => "Variable",
        _ => "Symbol",
    };

    let mut lines = vec![format!("**{}** `{}`", kind, symbol.name)];
    if let Some(namespace) = &symbol.namespace {
        if !namespace.is_empty() {
            lines.push(format!("**Namespace:** `{}`", namespace));
        }
    }

    if !template_params.is_empty() {
        lines.push(format!("**Templates:** `{}`", template_params.join(", ")));
    }

    if symbol.kind == SymbolKind::FUNCTION {
        let display_name = symbol_display_name_with_templates(&symbol.name, template_params);
        let signature =
            function_signature_label(&display_name, &symbol.parameters, symbol.return_type.as_deref());
        lines.push(format!("**Signature:** `{}`", signature));
    }

    lines.join("\n\n")
}

fn php_manual_function_url(identifier: &str) -> Option<String> {
    let normalized = identifier.trim_start_matches('\\');
    if normalized.is_empty() || normalized.starts_with('$') || normalized.contains('\\') {
        return None;
    }
    if !normalized
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return None;
    }

    let slug = normalized.to_lowercase().replace('_', "-");
    Some(format!("https://www.php.net/manual/en/function.{}.php", slug))
}

fn token_after_keyword(line: &str, keyword: &str) -> Option<String> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    let idx = parts.iter().position(|part| *part == keyword)?;
    let token = parts.get(idx + 1)?;
    let token = token.trim_start_matches('&').trim_start_matches('$');
    let cleaned: String = token
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '\\')
        .collect();

    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

fn parse_function_parameters(line: &str) -> Vec<String> {
    let Some(open) = line.find('(') else {
        return Vec::new();
    };
    let Some(close_rel) = line[open + 1..].find(')') else {
        return Vec::new();
    };
    let close = open + 1 + close_rel;
    let args = &line[(open + 1)..close];

    args.split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| p.to_string())
        .collect()
}

fn collect_function_declaration_signature(lines: &[&str], start_line: usize) -> String {
    let mut signature = String::new();
    let mut in_block_comment = false;
    let mut paren_depth = 0i32;
    let mut saw_open_paren = false;

    for line in lines.iter().skip(start_line).take(64) {
        let chars = line.chars().collect::<Vec<_>>();
        let mask = code_mask_for_line(&chars, &mut in_block_comment);
        let sanitized = chars
            .iter()
            .zip(mask.iter())
            .map(|(ch, is_code)| if *is_code { *ch } else { ' ' })
            .collect::<String>();
        let trimmed = sanitized.trim();
        if trimmed.is_empty() {
            continue;
        }

        if !signature.is_empty() {
            signature.push(' ');
        }
        signature.push_str(trimmed);

        let mut should_stop = false;
        for ch in trimmed.chars() {
            match ch {
                '(' => {
                    saw_open_paren = true;
                    paren_depth += 1;
                }
                ')' => {
                    if paren_depth > 0 {
                        paren_depth -= 1;
                    }
                }
                '{' | ';' => {
                    if saw_open_paren && paren_depth == 0 {
                        should_stop = true;
                        break;
                    }
                }
                _ => {}
            }
        }

        if should_stop {
            break;
        }
    }

    signature
}

fn parse_function_return_type(line: &str) -> Option<String> {
    let open = line.find('(')?;
    let close_rel = line[open + 1..].find(')')?;
    let close = open + 1 + close_rel;
    let after = line.get(close + 1..)?.trim_start();
    if !after.starts_with(':') {
        return None;
    }

    let return_part = after[1..].trim_start();
    if return_part.is_empty() {
        return None;
    }

    parse_type_annotation_prefix(return_part)
}

fn parse_type_annotation_prefix(input: &str) -> Option<String> {
    let mut angle_depth = 0i32;
    let mut paren_depth = 0i32;
    let mut out = String::new();

    for ch in input.chars() {
        if ch == '<' {
            angle_depth += 1;
        } else if ch == '>' {
            angle_depth = (angle_depth - 1).max(0);
        } else if ch == '(' {
            paren_depth += 1;
        } else if ch == ')' {
            paren_depth = (paren_depth - 1).max(0);
        }

        let is_type_char = ch.is_ascii_alphanumeric()
            || matches!(ch, '_' | '\\' | '|' | '?' | '[' | ']' | '<' | '>' | '&' | '(' | ')' | ',' | ':');

        if ch.is_whitespace() {
            if angle_depth == 0 && paren_depth == 0 {
                if out.is_empty() {
                    continue;
                }
                if out.trim_end().ends_with(':') {
                    out.push(' ');
                    continue;
                }
                break;
            }
            out.push(ch);
            continue;
        }

        if !is_type_char {
            break;
        }

        out.push(ch);
    }

    let parsed = out.trim();
    if parsed.is_empty() {
        None
    } else {
        Some(parsed.to_string())
    }
}

fn function_signature_label(name: &str, parameters: &[String], return_type: Option<&str>) -> String {
    let base = if parameters.is_empty() {
        format!("{}()", name)
    } else {
        format!("{}({})", name, parameters.join(", "))
    };

    if let Some(ret) = return_type {
        if !ret.is_empty() {
            return format!("{}: {}", base, ret);
        }
    }

    base
}

fn function_call_context(text: &str, position: Position) -> Option<(String, usize)> {
    let line = text.lines().nth(position.line as usize)?;
    let idx = (position.character as usize).min(line.len());
    let before = &line[..idx];

    let open_paren = before.rfind('(')?;
    let name_part = before[..open_paren].trim_end();
    let function_name = name_part
        .chars()
        .rev()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '\\')
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();

    if function_name.is_empty() {
        return None;
    }

    let params_slice = &before[(open_paren + 1)..];
    let active_parameter = params_slice.chars().filter(|ch| *ch == ',').count();
    Some((function_name, active_parameter))
}

fn completion_kind_from_symbol(kind: SymbolKind) -> CompletionItemKind {
    match kind {
        SymbolKind::CLASS => CompletionItemKind::CLASS,
        SymbolKind::INTERFACE => CompletionItemKind::INTERFACE,
        SymbolKind::FUNCTION => CompletionItemKind::FUNCTION,
        SymbolKind::CONSTANT => CompletionItemKind::CONSTANT,
        _ => CompletionItemKind::TEXT,
    }
}

fn static_member_completion_context(text: &str, position: Position) -> Option<String> {
    let line = text.split('\n').nth(position.line as usize)?;
    let cursor = (position.character as usize).min(line.chars().count());
    let chars = line.chars().collect::<Vec<_>>();
    let mut in_block_comment = false;
    let mask = code_mask_for_line(&chars, &mut in_block_comment);
    if cursor < 2 {
        return None;
    }

    let mut scope_idx = None;
    let mut i = cursor;
    while i >= 2 {
        if chars[i - 2] == ':'
            && chars[i - 1] == ':'
            && mask.get(i - 2).copied().unwrap_or(false)
            && mask.get(i - 1).copied().unwrap_or(false)
        {
            scope_idx = Some(i - 2);
            break;
        }
        i -= 1;
    }
    let scope_idx = scope_idx?;

    let mut start = scope_idx;
    while start > 0 {
        let ch = chars[start - 1];
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '\\' {
            start -= 1;
            continue;
        }
        break;
    }

    let class_name = chars[start..scope_idx].iter().collect::<String>();
    if class_name.is_empty() {
        return None;
    }

    let short = class_name.rsplit_once('\\').map(|(_, s)| s).unwrap_or(&class_name);
    if !looks_like_type_name(short) {
        return None;
    }

    Some(class_name)
}

fn instance_member_completion_context(text: &str, position: Position) -> Option<String> {
    let line = text.split('\n').nth(position.line as usize)?;
    let cursor = (position.character as usize).min(line.chars().count());
    let chars = line.chars().collect::<Vec<_>>();
    let mut in_block_comment = false;
    let mask = code_mask_for_line(&chars, &mut in_block_comment);
    if cursor < 2 {
        return None;
    }

    let mut arrow_idx = None;
    let mut i = cursor;
    while i >= 2 {
        if chars[i - 2] == '-'
            && chars[i - 1] == '>'
            && mask.get(i - 2).copied().unwrap_or(false)
            && mask.get(i - 1).copied().unwrap_or(false)
        {
            arrow_idx = Some(i - 2);
            break;
        }
        i -= 1;
    }
    let arrow_idx = arrow_idx?;

    let mut start = arrow_idx;
    while start > 0 {
        let ch = chars[start - 1];
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '$' {
            start -= 1;
            continue;
        }
        break;
    }

    let var_name = chars[start..arrow_idx].iter().collect::<String>();
    if !var_name.starts_with('$') || var_name.len() < 2 {
        return None;
    }
    Some(var_name)
}

fn infer_variable_class_before_position(text: &str, position: Position, variable_name: &str) -> Option<String> {
    let mut inferred = None;
    let mut in_block_comment = false;
    for (line_idx, line) in text.split('\n').enumerate() {
        if line_idx as u32 > position.line {
            break;
        }
        let chars = line.chars().collect::<Vec<_>>();
        let mask = code_mask_for_line(&chars, &mut in_block_comment);

        let scan_limit = if line_idx as u32 == position.line {
            (position.character as usize).min(line.len())
        } else {
            line.len()
        };

        if let Some(annotation_type) = extract_var_annotation_type_for_variable_from_line(
            line,
            &mask,
            variable_name,
            scan_limit,
        ) {
            inferred = Some(annotation_type);
        }

        let mut search_from = 0usize;
        while search_from < line.len() {
            let Some(found) = line[search_from..].find(variable_name) else {
                break;
            };
            let var_idx = search_from + found;
            search_from = var_idx + variable_name.len();

            let var_end = var_idx + variable_name.len();
            if mask
                .get(var_idx)
                .copied()
                .unwrap_or(false)
                == false
            {
                continue;
            }
            if line_idx as u32 == position.line && (var_idx as u32) >= position.character {
                continue;
            }

            let after_var = &line[var_end..];
            let Some(eq_rel_idx) = after_var.find('=') else {
                continue;
            };
            let eq_idx = var_end + eq_rel_idx;
            if line_idx as u32 == position.line && (eq_idx as u32) >= position.character {
                continue;
            }
            if eq_idx > 0 {
                let prev = chars[eq_idx - 1];
                if prev == '=' || prev == '!' || prev == '<' || prev == '>' {
                    continue;
                }
            }
            if eq_idx + 1 < chars.len() && chars[eq_idx + 1] == '=' {
                continue;
            }
            if !mask.get(eq_idx).copied().unwrap_or(false) {
                continue;
            }

            let after_eq = after_var[(eq_rel_idx + 1)..].trim_start();
            if !after_eq.to_ascii_lowercase().starts_with("new ") {
                continue;
            }

            let after_new = after_eq[4..].trim_start();
            let class_name = after_new
                .chars()
                .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '\\')
                .collect::<String>();
            if !class_name.is_empty() {
                inferred = Some(class_name);
            }
        }
    }
    inferred
}

fn infer_variable_type_annotation_before_position(
    text: &str,
    position: Position,
    variable_name: &str,
) -> Option<String> {
    let mut inferred = None;
    let mut in_block_comment = false;
    for (line_idx, line) in text.split('\n').enumerate() {
        if line_idx as u32 > position.line {
            break;
        }

        let chars = line.chars().collect::<Vec<_>>();
        let mask = code_mask_for_line(&chars, &mut in_block_comment);
        let scan_limit = if line_idx as u32 == position.line {
            (position.character as usize).min(line.len())
        } else {
            line.len()
        };

        if let Some(raw_type) = extract_var_annotation_raw_type_for_variable_from_line(
            line,
            &mask,
            variable_name,
            scan_limit,
        ) {
            inferred = Some(raw_type);
        }
    }

    inferred
}

fn extract_var_annotation_type_for_variable_from_line(
    line: &str,
    mask: &[bool],
    variable_name: &str,
    scan_limit: usize,
) -> Option<String> {
    let patterns = ["@var", "@psalm-var", "@phpstan-var"];

    for pattern in patterns {
        let mut search_start = 0usize;
        while search_start < scan_limit {
            let Some(found) = line[search_start..scan_limit].find(pattern) else {
                break;
            };
            let at = search_start + found;
            search_start = at + pattern.len();

            // @var annotations should come from comment regions, not executable code/strings.
            if mask.get(at).copied().unwrap_or(true) {
                continue;
            }
            if !is_likely_comment_annotation_position(line, at) {
                continue;
            }

            let tail = &line[(at + pattern.len())..scan_limit];
            let Some(name) = extract_first_variable_name(tail) else {
                continue;
            };
            if name != variable_name {
                continue;
            }

            let Some(raw_type) = parse_type_annotation_prefix(tail.trim_start()) else {
                continue;
            };
            if let Some(normalized) = normalize_type_name_for_inference(&raw_type) {
                return Some(normalized);
            }
        }
    }

    None
}

fn extract_var_annotation_raw_type_for_variable_from_line(
    line: &str,
    mask: &[bool],
    variable_name: &str,
    scan_limit: usize,
) -> Option<String> {
    let patterns = ["@var", "@psalm-var", "@phpstan-var"];

    for pattern in patterns {
        let mut search_start = 0usize;
        while search_start < scan_limit {
            let Some(found) = line[search_start..scan_limit].find(pattern) else {
                break;
            };
            let at = search_start + found;
            search_start = at + pattern.len();

            if mask.get(at).copied().unwrap_or(true) {
                continue;
            }
            if !is_likely_comment_annotation_position(line, at) {
                continue;
            }

            let tail = &line[(at + pattern.len())..scan_limit];
            let Some(name) = extract_first_variable_name(tail) else {
                continue;
            };
            if name != variable_name {
                continue;
            }

            let Some(raw_type) = parse_type_annotation_prefix(tail.trim_start()) else {
                continue;
            };

            return Some(raw_type.trim_start_matches('\\').to_string());
        }
    }

    None
}

fn parse_generic_type_instance(raw_type: &str) -> Option<(String, Vec<String>)> {
    let cleaned = raw_type.trim().trim_start_matches('?').trim_start_matches('\\');
    if !has_balanced_type_delimiters(cleaned) {
        return None;
    }
    let open = cleaned.find('<')?;
    let close = cleaned.rfind('>')?;
    if close <= open {
        return None;
    }

    let base = cleaned[..open].trim();
    if base.is_empty() {
        return None;
    }

    let inner = &cleaned[(open + 1)..close];
    let args = split_top_level_commas(inner)
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();

    if args.is_empty() {
        return None;
    }

    Some((base.to_string(), args))
}

fn has_balanced_type_delimiters(input: &str) -> bool {
    let mut angle = 0i32;
    let mut paren = 0i32;
    let mut bracket = 0i32;
    let mut brace = 0i32;

    for ch in input.chars() {
        match ch {
            '<' => angle += 1,
            '>' => {
                angle -= 1;
                if angle < 0 {
                    return false;
                }
            }
            '(' => paren += 1,
            ')' => {
                paren -= 1;
                if paren < 0 {
                    return false;
                }
            }
            '[' => bracket += 1,
            ']' => {
                bracket -= 1;
                if bracket < 0 {
                    return false;
                }
            }
            '{' => brace += 1,
            '}' => {
                brace -= 1;
                if brace < 0 {
                    return false;
                }
            }
            _ => {}
        }
    }

    angle == 0 && paren == 0 && bracket == 0 && brace == 0
}

fn apply_template_substitution(type_hint: &str, mapping: &BTreeMap<String, String>) -> String {
    if mapping.is_empty() {
        return type_hint.to_string();
    }

    let mut out = String::new();
    let chars = type_hint.chars().collect::<Vec<_>>();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i].is_ascii_alphabetic() || chars[i] == '_' {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let token = chars[start..i].iter().collect::<String>();
            if let Some(replacement) = mapping.get(&token) {
                out.push_str(replacement);
            } else {
                out.push_str(&token);
            }
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }

    out
}

fn is_likely_comment_annotation_position(line: &str, at: usize) -> bool {
    let prefix = &line[..at.min(line.len())];
    let chars = prefix.chars().collect::<Vec<_>>();
    let mut i = chars.len();
    while i > 0 {
        i -= 1;
        if chars[i].is_whitespace() {
            continue;
        }
        return matches!(chars[i], '*' | '/' | '#');
    }
    false
}

fn normalize_type_name_for_inference(raw_type: &str) -> Option<String> {
    for union_part in raw_type.split('|') {
        for intersection_part in union_part.split('&') {
            let mut part = intersection_part.trim();
            if part.is_empty() {
                continue;
            }

            part = part.trim_start_matches('?').trim_start_matches('\\');
            if let Some(generic_start) = part.find('<') {
                part = &part[..generic_start];
            }
            while let Some(stripped) = part.strip_suffix("[]") {
                part = stripped;
            }

            let lower = part.to_ascii_lowercase();
            if matches!(
                lower.as_str(),
                "null"
                    | "mixed"
                    | "bool"
                    | "boolean"
                    | "int"
                    | "integer"
                    | "float"
                    | "string"
                    | "array"
                    | "object"
                    | "callable"
                    | "iterable"
                    | "resource"
                    | "void"
                    | "never"
                    | "true"
                    | "false"
                    | "self"
                    | "static"
                    | "parent"
            ) {
                continue;
            }

            if part.is_empty() {
                continue;
            }
            if !part
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '\\')
            {
                continue;
            }

            let short = part.rsplit_once('\\').map(|(_, s)| s).unwrap_or(part);
            if looks_like_type_name(short) {
                return Some(part.to_string());
            }
        }
    }

    None
}

fn parse_property_type_hint_from_declaration_head(head: &str) -> Option<String> {
    let filtered = head
        .split_whitespace()
        .filter(|token| {
            !matches!(
                token.to_ascii_lowercase().as_str(),
                "public"
                    | "private"
                    | "protected"
                    | "static"
                    | "readonly"
                    | "final"
                    | "var"
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    if filtered.trim().is_empty() {
        return None;
    }
    parse_type_annotation_prefix(filtered.trim()).map(|ty| ty.trim_start_matches('\\').to_string())
}

fn collect_class_member_labels(text: &str, class_symbol: &PhpSymbol) -> Vec<(String, CompletionItemKind)> {
    collect_class_member_entries(text, class_symbol)
        .into_iter()
        .map(|entry| (entry.label, entry.kind))
        .collect()
}

#[derive(Clone)]
struct ClassMemberEntry {
    label: String,
    kind: CompletionItemKind,
    is_static: bool,
    type_hint: Option<String>,
}

fn collect_class_member_entries(text: &str, class_symbol: &PhpSymbol) -> Vec<ClassMemberEntry> {
    if !is_type_symbol_kind(class_symbol.kind) {
        return Vec::new();
    }

    let lines = text.split('\n').collect::<Vec<_>>();
    let start_line = class_symbol.range.start.line as usize;
    if start_line >= lines.len() {
        return Vec::new();
    }

    let mut open_line = None;
    for (idx, line) in lines.iter().enumerate().skip(start_line) {
        if line.contains('{') {
            open_line = Some(idx);
            break;
        }
    }
    let Some(open_line) = open_line else {
        return Vec::new();
    };

    let mut depth = 0i32;
    let mut members = Vec::new();
    let mut seen = HashSet::new();
    let mut in_block_comment = false;
    for (line_idx, line) in lines.iter().enumerate().skip(open_line) {
        let trimmed = line.trim_start();
        let chars = line.chars().collect::<Vec<_>>();
        let mask = code_mask_for_line(&chars, &mut in_block_comment);
        if depth == 1 {
            if let Some(name) = token_after_keyword(trimmed, "function") {
                if name == "__construct" {
                    for promoted in extract_promoted_property_entries_from_lines(&lines, line_idx) {
                        if seen.insert(format!("p:{}", promoted.name)) {
                            members.push(ClassMemberEntry {
                                label: promoted.name,
                                kind: CompletionItemKind::FIELD,
                                is_static: false,
                                type_hint: promoted.type_hint,
                            });
                        }
                    }
                }
                let signature = collect_function_declaration_signature(&lines, line_idx);
                if seen.insert(format!("m:{name}")) {
                    members.push(ClassMemberEntry {
                        label: name,
                        kind: CompletionItemKind::METHOD,
                        is_static: trimmed.contains(" static ") || trimmed.starts_with("static "),
                        type_hint: parse_function_return_type(&signature).or_else(|| {
                            get_nearby_docblock(&lines, line_idx)
                                .and_then(|docblock| extract_return_from_docblock(&docblock))
                        }),
                    });
                }
            }
            if let Some(col) = trimmed.find('$') {
                let tail = &trimmed[col..];
                let declaration_head = &trimmed[..col];
                let property_type = parse_property_type_hint_from_declaration_head(declaration_head.trim());
                for segment in tail.split(',') {
                    if let Some(name) = extract_first_variable_name(segment) {
                        let prop = name.trim_start_matches('$').to_string();
                        if !prop.is_empty() && seen.insert(format!("p:{prop}")) {
                            members.push(ClassMemberEntry {
                                label: prop,
                                kind: CompletionItemKind::FIELD,
                                is_static: declaration_head.contains("static"),
                                type_hint: property_type.clone(),
                            });
                        }
                    }
                }
            }
        }

        for (idx, ch) in chars.iter().enumerate() {
            if !mask.get(idx).copied().unwrap_or(false) {
                continue;
            }
            if *ch == '{' {
                depth += 1;
            } else if *ch == '}' {
                depth -= 1;
            }
        }
        if depth <= 0 && !members.is_empty() {
            break;
        }
    }

    members
}

fn extract_promoted_property_names(line: &str) -> Vec<String> {
    extract_promoted_property_names_from_lines(&[line], 0)
}

fn extract_promoted_property_names_from_lines(lines: &[&str], start_line: usize) -> Vec<String> {
    extract_promoted_property_entries_from_lines(lines, start_line)
        .into_iter()
        .map(|entry| entry.name)
        .collect()
}

#[derive(Clone)]
struct PromotedPropertyEntry {
    name: String,
    type_hint: Option<String>,
}

fn extract_promoted_property_entries_from_lines(
    lines: &[&str],
    start_line: usize,
) -> Vec<PromotedPropertyEntry> {
    let mut entries = Vec::new();
    if start_line >= lines.len() {
        return entries;
    }

    let mut found_open = false;
    let mut depth = 0i32;
    let mut inside = String::new();

    for line in lines.iter().skip(start_line) {
        for ch in line.chars() {
            if !found_open {
                if ch == '(' {
                    found_open = true;
                    depth = 1;
                }
                continue;
            }

            if ch == '(' {
                depth += 1;
                inside.push(ch);
                continue;
            }
            if ch == ')' {
                depth -= 1;
                if depth == 0 {
                    for param in split_top_level_commas(&inside) {
                        let trimmed = param.trim();
                        if !(trimmed.contains("public ")
                            || trimmed.contains("protected ")
                            || trimmed.contains("private "))
                        {
                            continue;
                        }

                        let Some(var_name) = extract_first_variable_name(trimmed) else {
                            continue;
                        };
                        let name = var_name.trim_start_matches('$').to_string();
                        if name.is_empty() {
                            continue;
                        }

                        let var_idx = trimmed.find(&var_name).unwrap_or(trimmed.len());
                        let declaration_head = trimmed[..var_idx].trim();
                        let type_hint = parse_property_type_hint_from_declaration_head(declaration_head);

                        entries.push(PromotedPropertyEntry { name, type_hint });
                    }
                    return entries;
                }
                inside.push(ch);
                continue;
            }

            inside.push(ch);
        }

        if found_open {
            inside.push(' ');
        }
    }

    entries
}

fn split_top_level_commas(input: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut angle = 0i32;
    let mut paren = 0i32;
    let mut bracket = 0i32;
    let mut brace = 0i32;

    for ch in input.chars() {
        match ch {
            '<' => {
                angle += 1;
                current.push(ch);
            }
            '>' => {
                angle = (angle - 1).max(0);
                current.push(ch);
            }
            '(' => {
                paren += 1;
                current.push(ch);
            }
            ')' => {
                paren = (paren - 1).max(0);
                current.push(ch);
            }
            '[' => {
                bracket += 1;
                current.push(ch);
            }
            ']' => {
                bracket = (bracket - 1).max(0);
                current.push(ch);
            }
            '{' => {
                brace += 1;
                current.push(ch);
            }
            '}' => {
                brace = (brace - 1).max(0);
                current.push(ch);
            }
            ',' if angle == 0 && paren == 0 && bracket == 0 && brace == 0 => {
                parts.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }

    let tail = current.trim();
    if !tail.is_empty() {
        parts.push(tail.to_string());
    }

    parts
}

fn php_magic_constants() -> [&'static str; 8] {
    [
        "__CLASS__",
        "__DIR__",
        "__FILE__",
        "__FUNCTION__",
        "__LINE__",
        "__METHOD__",
        "__NAMESPACE__",
        "__TRAIT__",
    ]
}

fn completion_score(
    label: &str,
    lowercase_prefix: &str,
    is_local: bool,
    kind: Option<CompletionItemKind>,
    context: CompletionContextKind,
) -> i32 {
    let mut score = 0;
    let label_lower = label.to_lowercase();

    if lowercase_prefix.is_empty() {
        score += 10;
    } else if label_lower == lowercase_prefix {
        score += 120;
    } else if label_lower.starts_with(lowercase_prefix) {
        score += 90;
    }

    if is_local {
        score += 30;
    }

    score += match kind {
        Some(CompletionItemKind::FUNCTION) => 14,
        Some(CompletionItemKind::METHOD) => 14,
        Some(CompletionItemKind::CLASS) => 12,
        Some(CompletionItemKind::INTERFACE) => 12,
        Some(CompletionItemKind::CONSTANT) => 10,
        Some(CompletionItemKind::VARIABLE) => 12,
        Some(CompletionItemKind::KEYWORD) => 6,
        _ => 0,
    };

    if context == CompletionContextKind::UseStatement {
        score += match kind {
            Some(CompletionItemKind::CLASS)
            | Some(CompletionItemKind::INTERFACE)
            | Some(CompletionItemKind::MODULE) => 35,
            Some(CompletionItemKind::VARIABLE) => -40,
            Some(CompletionItemKind::KEYWORD) => -30,
            _ => 0,
        };
    }

    score
}

fn completion_context_kind(text: &str, position: Position) -> CompletionContextKind {
    let Some(line) = text.lines().nth(position.line as usize) else {
        return CompletionContextKind::General;
    };

    let idx = (position.character as usize).min(line.len());
    let prefix = &line[..idx];
    if prefix.trim_start().starts_with("use ") {
        return CompletionContextKind::UseStatement;
    }

    CompletionContextKind::General
}

fn blade_directive_keywords() -> Vec<&'static str> {
    vec![
        "@if",
        "@elseif",
        "@else",
        "@endif",
        "@foreach",
        "@endforeach",
        "@for",
        "@endfor",
        "@while",
        "@endwhile",
        "@section",
        "@endsection",
        "@yield",
        "@extends",
        "@include",
        "@csrf",
        "@method",
    ]
}

fn laravel_facade_names() -> Vec<&'static str> {
    vec![
        "Route", "DB", "Cache", "Config", "Auth", "Gate", "Event", "Log", "Queue",
        "Storage", "Http", "Schema", "Artisan", "Mail", "Notification", "Session",
    ]
}

fn laravel_helper_functions() -> Vec<&'static str> {
    vec![
        "route", "config", "app", "resolve", "trans", "__", "view", "redirect",
        "response", "request", "collect", "now", "old", "asset", "url",
    ]
}

fn laravel_eloquent_static_methods() -> Vec<&'static str> {
    vec!["query", "where", "find", "first", "create", "all", "with", "withCount"]
}

fn laravel_eloquent_instance_methods() -> Vec<&'static str> {
    vec!["save", "update", "delete", "refresh", "load", "loadCount", "toArray"]
}

fn laravel_string_completion_context(text: &str, position: Position, function_name: &str) -> Option<char> {
    let line = text.lines().nth(position.line as usize)?;
    let idx = (position.character as usize).min(line.len());
    let prefix = &line[..idx];

    let single = format!("{}('", function_name);
    let double = format!("{}(\"", function_name);
    if prefix.contains(&single) {
        return Some('\'');
    }
    if prefix.contains(&double) {
        return Some('"');
    }

    None
}

fn collect_laravel_route_names(docs: &HashMap<Url, String>) -> Vec<String> {
    let mut names = HashSet::new();
    for text in docs.values() {
        names.extend(extract_laravel_route_names_from_text(text));
    }
    let mut out = names.into_iter().collect::<Vec<_>>();
    out.sort();
    out
}

fn extract_laravel_route_names_from_text(text: &str) -> HashSet<String> {
    let mut names = HashSet::new();
    let mut rest = text;
    while let Some(idx) = rest.find("->name(") {
        let after = &rest[(idx + "->name(".len())..];
        if let Some((name, consumed)) = parse_quoted_string_literal(after) {
            if !name.is_empty() {
                names.insert(name);
            }
            rest = &after[consumed..];
            continue;
        }
        rest = after;
    }
    names
}

fn collect_laravel_config_keys(docs: &HashMap<Url, String>) -> Vec<String> {
    let mut keys = HashSet::new();

    for (uri, text) in docs {
        let path = uri.path().replace('\\', "/").to_ascii_lowercase();
        if !path.contains("/config/") || !path.ends_with(".php") {
            continue;
        }

        let file_name = path
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .trim_end_matches(".php");
        if file_name.is_empty() {
            continue;
        }

        for key in extract_php_array_keys(text) {
            keys.insert(format!("{file_name}.{key}"));
        }
    }

    let mut out = keys.into_iter().collect::<Vec<_>>();
    out.sort();
    out
}

fn extract_php_array_keys(text: &str) -> HashSet<String> {
    let mut keys = HashSet::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0usize;

    while i < chars.len() {
        if chars[i] != '\'' && chars[i] != '"' {
            i += 1;
            continue;
        }

        let quote = chars[i];
        let start = i + 1;
        i += 1;
        while i < chars.len() {
            if chars[i] == '\\' {
                i = (i + 2).min(chars.len());
                continue;
            }
            if chars[i] == quote {
                break;
            }
            i += 1;
        }
        if i >= chars.len() {
            break;
        }

        let key = chars[start..i].iter().collect::<String>();
        i += 1;

        let mut j = i;
        while j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        if j + 1 < chars.len() && chars[j] == '=' && chars[j + 1] == '>' {
            if key
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '.')
            {
                keys.insert(key);
            }
        }
    }

    keys
}

fn parse_quoted_string_literal(input: &str) -> Option<(String, usize)> {
    let chars: Vec<char> = input.chars().collect();
    if chars.is_empty() {
        return None;
    }
    let quote = chars[0];
    if quote != '\'' && quote != '"' {
        return None;
    }

    let mut i = 1usize;
    while i < chars.len() {
        if chars[i] == '\\' {
            i = (i + 2).min(chars.len());
            continue;
        }
        if chars[i] == quote {
            let value = chars[1..i].iter().collect::<String>();
            return Some((value, i + 1));
        }
        i += 1;
    }
    None
}

fn extract_local_variables_before_position(text: &str, position: Position) -> Vec<String> {
    let mut vars = HashSet::new();
    let stop_line = position.line as usize;

    for (line_idx, line) in text.lines().enumerate() {
        if line_idx > stop_line {
            break;
        }

        let max_col = if line_idx == stop_line {
            (position.character as usize).min(line.len())
        } else {
            line.len()
        };
        let slice = &line[..max_col];

        let chars: Vec<char> = slice.chars().collect();
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
                vars.insert(name);
            }
        }
    }

    let mut out: Vec<String> = vars.into_iter().collect();
    out.sort();
    out
}

fn identifier_at_position(text: &str, position: Position) -> Option<String> {
    identifier_and_range_at_position(text, position).map(|(ident, _)| ident)
}

fn identifier_and_range_at_position(text: &str, position: Position) -> Option<(String, Range)> {
    let line = text.lines().nth(position.line as usize)?;
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        return None;
    }

    let mut idx = position.character as usize;
    if idx >= chars.len() {
        idx = chars.len().saturating_sub(1);
    }

    if !is_identifier_char(chars[idx]) {
        if idx == 0 || !is_identifier_char(chars[idx - 1]) {
            return None;
        }
        idx -= 1;
    }

    let mut start = idx;
    while start > 0 && is_identifier_char(chars[start - 1]) {
        start -= 1;
    }

    let mut end = idx;
    while end + 1 < chars.len() && is_identifier_char(chars[end + 1]) {
        end += 1;
    }

    let ident: String = chars[start..=end].iter().collect();
    if ident.is_empty() {
        None
    } else {
        Some((
            ident,
            Range::new(
                Position::new(position.line, start as u32),
                Position::new(position.line, (end + 1) as u32),
            ),
        ))
    }
}

fn selection_ranges_for_positions(text: &str, positions: &[Position]) -> Vec<SelectionRange> {
    let mut out = Vec::new();

    for position in positions {
        let Some(line_text) = text.lines().nth(position.line as usize) else {
            continue;
        };

        let line_range = Range::new(
            Position::new(position.line, 0),
            Position::new(position.line, line_text.chars().count() as u32),
        );

        if let Some((_, identifier_range)) = identifier_and_range_at_position(text, *position) {
            out.push(SelectionRange {
                range: identifier_range,
                parent: Some(Box::new(SelectionRange {
                    range: line_range,
                    parent: None,
                })),
            });
            continue;
        }

        out.push(SelectionRange {
            range: line_range,
            parent: None,
        });
    }

    out
}

fn identifier_prefix_at_position(text: &str, position: Position) -> Option<String> {
    let line = text.lines().nth(position.line as usize)?;
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        return Some(String::new());
    }

    let mut idx = position.character as usize;
    if idx > chars.len() {
        idx = chars.len();
    }

    let mut start = idx;
    while start > 0 && is_identifier_char(chars[start - 1]) {
        start -= 1;
    }

    let prefix: String = chars[start..idx].iter().collect();
    Some(prefix.trim_start_matches('$').to_string())
}

fn is_identifier_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || ch == '\\' || ch == '$'
}

fn is_valid_identifier_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };

    if !(first.is_ascii_alphabetic() || first == '_' || first == '\\' || first == '$') {
        return false;
    }

    chars.all(is_identifier_char)
}

fn find_identifier_ranges(text: &str, target: &str) -> Vec<Range> {
    if target.is_empty() {
        return Vec::new();
    }

    let mut ranges = Vec::new();
    let target_chars: Vec<char> = target.chars().collect();
    let target_len = target_chars.len();
    let mut in_block_comment = false;

    for (line_idx, line) in text.lines().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        if chars.len() < target_len {
            // Keep block comment state up-to-date even for short lines.
            let _ = code_mask_for_line(&chars, &mut in_block_comment);
            continue;
        }

        let code_mask = code_mask_for_line(&chars, &mut in_block_comment);

        for start in 0..=(chars.len() - target_len) {
            let end_exclusive = start + target_len;
            if chars[start..end_exclusive] != target_chars[..] {
                continue;
            }

            if !code_mask[start..end_exclusive].iter().all(|v| *v) {
                continue;
            }

            let left_ok = start == 0 || !is_identifier_char(chars[start - 1]);
            let right_ok = end_exclusive == chars.len() || !is_identifier_char(chars[end_exclusive]);
            if !left_ok || !right_ok {
                continue;
            }

            ranges.push(Range::new(
                Position::new(line_idx as u32, start as u32),
                Position::new(line_idx as u32, end_exclusive as u32),
            ));
        }
    }

    ranges
}

fn is_code_position(text: &str, position: Position) -> bool {
    let mut in_block_comment = false;

    for (line_idx, line) in text.lines().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let mask = code_mask_for_line(&chars, &mut in_block_comment);

        if line_idx as u32 != position.line {
            continue;
        }

        if chars.is_empty() {
            return false;
        }

        let mut idx = position.character as usize;
        if idx >= chars.len() {
            idx = chars.len().saturating_sub(1);
        }

        return mask.get(idx).copied().unwrap_or(false);
    }

    false
}

fn code_mask_for_line(chars: &[char], in_block_comment: &mut bool) -> Vec<bool> {
    let mut mask = vec![true; chars.len()];
    let mut i = 0usize;
    let mut in_single = false;
    let mut in_double = false;

    while i < chars.len() {
        if *in_block_comment {
            mask[i] = false;
            if i + 1 < chars.len() && chars[i] == '*' && chars[i + 1] == '/' {
                mask[i + 1] = false;
                *in_block_comment = false;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }

        if in_single {
            mask[i] = false;
            if chars[i] == '\\' && i + 1 < chars.len() {
                mask[i + 1] = false;
                i += 2;
                continue;
            }
            if chars[i] == '\'' {
                in_single = false;
            }
            i += 1;
            continue;
        }

        if in_double {
            mask[i] = false;
            if chars[i] == '\\' && i + 1 < chars.len() {
                mask[i + 1] = false;
                i += 2;
                continue;
            }
            if chars[i] == '"' {
                in_double = false;
            }
            i += 1;
            continue;
        }

        if i + 1 < chars.len() && chars[i] == '/' && chars[i + 1] == '*' {
            mask[i] = false;
            mask[i + 1] = false;
            *in_block_comment = true;
            i += 2;
            continue;
        }

        if i + 1 < chars.len() && chars[i] == '/' && chars[i + 1] == '/' {
            for j in i..chars.len() {
                mask[j] = false;
            }
            break;
        }

        if chars[i] == '#' {
            for j in i..chars.len() {
                mask[j] = false;
            }
            break;
        }

        if chars[i] == '\'' {
            mask[i] = false;
            in_single = true;
            i += 1;
            continue;
        }

        if chars[i] == '"' {
            mask[i] = false;
            in_double = true;
            i += 1;
            continue;
        }

        i += 1;
    }

    mask
}

fn comment_text_for_line(chars: &[char], in_block_comment: &mut bool) -> String {
    let mut out = vec![' '; chars.len()];
    let mut i = 0usize;
    let mut in_single = false;
    let mut in_double = false;

    while i < chars.len() {
        if *in_block_comment {
            out[i] = chars[i];
            if i + 1 < chars.len() && chars[i] == '*' && chars[i + 1] == '/' {
                out[i + 1] = chars[i + 1];
                *in_block_comment = false;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }

        if in_single {
            if chars[i] == '\\' && i + 1 < chars.len() {
                i += 2;
                continue;
            }
            if chars[i] == '\'' {
                in_single = false;
            }
            i += 1;
            continue;
        }

        if in_double {
            if chars[i] == '\\' && i + 1 < chars.len() {
                i += 2;
                continue;
            }
            if chars[i] == '"' {
                in_double = false;
            }
            i += 1;
            continue;
        }

        if i + 1 < chars.len() && chars[i] == '/' && chars[i + 1] == '*' {
            out[i] = chars[i];
            out[i + 1] = chars[i + 1];
            *in_block_comment = true;
            i += 2;
            continue;
        }

        if i + 1 < chars.len() && chars[i] == '/' && chars[i + 1] == '/' {
            for j in i..chars.len() {
                out[j] = chars[j];
            }
            break;
        }

        if chars[i] == '#' {
            for j in i..chars.len() {
                out[j] = chars[j];
            }
            break;
        }

        if chars[i] == '\'' {
            in_single = true;
            i += 1;
            continue;
        }

        if chars[i] == '"' {
            in_double = true;
            i += 1;
            continue;
        }

        i += 1;
    }

    out.into_iter().collect()
}



#[cfg(test)]
mod tests;

#[tokio::main]
async fn main() -> Result<()> {
    let (service, socket) = LspService::new(|client| Backend {
        client,
        documents: RwLock::new(HashMap::new()),
        symbols: RwLock::new(HashMap::new()),
        workspace_folders: RwLock::new(Vec::new()),
        open_documents: RwLock::new(HashSet::new()),
    });
    Server::new(stdin(), stdout(), socket).serve(service).await;
    Ok(())
}
