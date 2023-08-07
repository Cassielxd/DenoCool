// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

use std::path::PathBuf;
use std::sync::Arc;

use deno_core::error::AnyError;
use deno_core::futures::FutureExt;
use deno_graph::Module;
use deno_runtime::colors;

use crate::args::BundleFlags;
use crate::args::CliOptions;
use crate::args::Flags;
use crate::args::TsConfigType;
use crate::args::TypeCheckMode;
use crate::factory::CliFactory;
use crate::graph_util::error_for_any_npm_specifier;
use crate::util;
use crate::util::display;
use crate::util::file_watcher::ResolutionResult;

pub async fn bundle(flags: Flags, bundle_flags: BundleFlags) -> Result<(), AnyError> {
  let cli_options = Arc::new(CliOptions::from_flags(flags)?);

  log::info!(
    "{} \"deno bundle\" is deprecated and will be removed in the future.",
    colors::yellow("Warning"),
  );
  log::info!("Use alternative bundlers like \"deno_emit\", \"esbuild\" or \"rollup\" instead.");

  let module_specifier = cli_options.resolve_main_module()?;

  let resolver = |_| {
    let cli_options = cli_options.clone();
    let module_specifier = &module_specifier;
    async move {
      log::debug!(">>>>> bundle START");
      let factory = CliFactory::from_cli_options(cli_options);
      let module_graph_builder = factory.module_graph_builder().await?;
      let cli_options = factory.cli_options();

      let graph = module_graph_builder.create_graph_and_maybe_check(vec![module_specifier.clone()]).await?;

      let mut paths_to_watch: Vec<PathBuf> = graph
        .specifiers()
        .filter_map(|(_, r)| {
          r.ok().and_then(|module| match module {
            Module::Esm(m) => m.specifier.to_file_path().ok(),
            Module::Json(m) => m.specifier.to_file_path().ok(),
            // nothing to watch
            Module::Node(_) | Module::Npm(_) | Module::External(_) => None,
          })
        })
        .collect();

      if let Ok(Some(import_map_path)) = cli_options
        .resolve_import_map_specifier()
        .map(|ms| ms.and_then(|ref s| s.to_file_path().ok()))
      {
        paths_to_watch.push(import_map_path);
      }

      Ok((paths_to_watch, graph, cli_options.clone()))
    }
    .map(move |result| match result {
      Ok((paths_to_watch, graph, ps)) => ResolutionResult::Restart {
        paths_to_watch,
        result: Ok((ps, graph)),
      },
      Err(e) => ResolutionResult::Restart {
        paths_to_watch: vec![module_specifier.to_file_path().unwrap()],
        result: Err(e),
      },
    })
  };

  let operation = |(cli_options, graph): (Arc<CliOptions>, Arc<deno_graph::ModuleGraph>)| {
    let out_file = &bundle_flags.out_file;
    async move {
      // at the moment, we don't support npm specifiers in deno bundle, so show an error
      error_for_any_npm_specifier(&graph)?;

      let bundle_output = bundle_module_graph(graph.as_ref(), &cli_options)?;
      log::debug!(">>>>> bundle END");

      if let Some(out_file) = out_file {
        let output_bytes = bundle_output.code.as_bytes();
        let output_len = output_bytes.len();
        util::fs::write_file(out_file, output_bytes, 0o644)?;
        log::info!(
          "{} {:?} ({})",
          colors::green("Emit"),
          out_file,
          colors::gray(display::human_size(output_len as f64))
        );
        if let Some(bundle_map) = bundle_output.maybe_map {
          let map_bytes = bundle_map.as_bytes();
          let map_len = map_bytes.len();
          let ext = if let Some(curr_ext) = out_file.extension() {
            format!("{}.map", curr_ext.to_string_lossy())
          } else {
            "map".to_string()
          };
          let map_out_file = out_file.with_extension(ext);
          util::fs::write_file(&map_out_file, map_bytes, 0o644)?;
          log::info!(
            "{} {:?} ({})",
            colors::green("Emit"),
            map_out_file,
            colors::gray(display::human_size(map_len as f64))
          );
        }
      } else {
        println!("{}", bundle_output.code);
      }

      Ok(())
    }
  };

  if cli_options.watch_paths().is_some() {
    util::file_watcher::watch_func(
      resolver,
      operation,
      util::file_watcher::PrintConfig {
        job_name: "Bundle".to_string(),
        clear_screen: !cli_options.no_clear_screen(),
      },
    )
    .await?;
  } else {
    let module_graph = if let ResolutionResult::Restart { result, .. } = resolver(None).await {
      result?
    } else {
      unreachable!();
    };
    operation(module_graph).await?;
  }

  Ok(())
}

fn bundle_module_graph(graph: &deno_graph::ModuleGraph, cli_options: &CliOptions) -> Result<deno_emit::BundleEmit, AnyError> {
  log::info!("{} {}", colors::green("Bundle"), graph.roots[0]);

  let ts_config_result = cli_options.resolve_ts_config_for_emit(TsConfigType::Bundle)?;
  if cli_options.type_check_mode() == TypeCheckMode::None {
    if let Some(ignored_options) = ts_config_result.maybe_ignored_options {
      log::warn!("{}", ignored_options);
    }
  }

  deno_emit::bundle_graph(
    graph,
    deno_emit::BundleOptions {
      bundle_type: deno_emit::BundleType::Module,
      emit_options: ts_config_result.ts_config.into(),
      emit_ignore_directives: true,
    },
  )
}
