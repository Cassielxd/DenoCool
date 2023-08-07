// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

use crate::args::DocFlags;
use crate::args::DocSourceFileFlag;
use crate::args::Flags;
use crate::colors;
use crate::display::write_json_to_stdout;
use crate::display::write_to_stdout_ignore_sigpipe;
use crate::factory::CliFactory;
use crate::file_fetcher::File;
use crate::graph_util::graph_lock_or_exit;
use crate::tsc::get_types_declaration_file_text;
use deno_ast::MediaType;
use deno_core::anyhow::bail;
use deno_core::error::AnyError;
use deno_core::resolve_path;
use deno_core::resolve_url_or_path;
use deno_doc as doc;
use deno_graph::ModuleSpecifier;
use std::path::PathBuf;

pub async fn print_docs(flags: Flags, doc_flags: DocFlags) -> Result<(), AnyError> {
  let factory = CliFactory::from_flags(flags).await?;
  let cli_options = factory.cli_options();

  let mut doc_nodes = match doc_flags.source_file {
    DocSourceFileFlag::Builtin => {
      let source_file_specifier = ModuleSpecifier::parse("internal://lib.deno.d.ts").unwrap();
      let content = get_types_declaration_file_text(cli_options.unstable());
      let mut loader = deno_graph::source::MemoryLoader::new(
        vec![(
          source_file_specifier.to_string(),
          deno_graph::source::Source::Module {
            specifier: source_file_specifier.to_string(),
            content,
            maybe_headers: None,
          },
        )],
        Vec::new(),
      );
      let analyzer = deno_graph::CapturingModuleAnalyzer::default();
      let mut graph = deno_graph::ModuleGraph::default();
      graph
        .build(
          vec![source_file_specifier.clone()],
          &mut loader,
          deno_graph::BuildOptions {
            module_analyzer: Some(&analyzer),
            ..Default::default()
          },
        )
        .await;
      let doc_parser = doc::DocParser::new(graph, doc_flags.private, analyzer.as_capturing_parser());
      doc_parser.parse_module(&source_file_specifier)?.definitions
    }
    DocSourceFileFlag::Path(source_file) => {
      let file_fetcher = factory.file_fetcher()?;
      let module_graph_builder = factory.module_graph_builder().await?;
      let maybe_lockfile = factory.maybe_lockfile();
      let parsed_source_cache = factory.parsed_source_cache()?;

      let module_specifier = resolve_url_or_path(&source_file, cli_options.initial_cwd())?;

      // If the root module has external types, the module graph won't redirect it,
      // so instead create a dummy file which exports everything from the actual file being documented.
      let root_specifier = resolve_path("./$deno$doc.ts", cli_options.initial_cwd()).unwrap();
      let root = File {
        local: PathBuf::from("./$deno$doc.ts"),
        maybe_types: None,
        media_type: MediaType::TypeScript,
        source: format!("export * from \"{module_specifier}\";").into(),
        specifier: root_specifier.clone(),
        maybe_headers: None,
      };

      // Save our fake file into file fetcher cache.
      file_fetcher.insert_cached(root);

      let graph = module_graph_builder.create_graph(vec![root_specifier.clone()]).await?;

      if let Some(lockfile) = maybe_lockfile {
        graph_lock_or_exit(&graph, &mut lockfile.lock());
      }

      let doc_parser = doc::DocParser::new(graph, doc_flags.private, parsed_source_cache.as_capturing_parser());
      doc_parser.parse_with_reexports(&root_specifier)?
    }
  };

  if doc_flags.json {
    write_json_to_stdout(&doc_nodes)
  } else {
    doc_nodes.retain(|doc_node| doc_node.kind != doc::DocNodeKind::Import);
    let details = if let Some(filter) = doc_flags.filter {
      let nodes = doc::find_nodes_by_name_recursively(doc_nodes, filter.clone());
      if nodes.is_empty() {
        bail!("Node {} was not found!", filter);
      }
      format!("{}", doc::DocPrinter::new(&nodes, colors::use_color(), doc_flags.private))
    } else {
      format!("{}", doc::DocPrinter::new(&doc_nodes, colors::use_color(), doc_flags.private))
    };

    write_to_stdout_ignore_sigpipe(details.as_bytes()).map_err(AnyError::from)
  }
}
