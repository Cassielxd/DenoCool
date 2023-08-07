use crate::util;

use deno_ast::ModuleSpecifier;
use deno_core::error::AnyError;
use deno_core::Extension;
use deno_runtime::permissions::PermissionsContainer;
use tokio::net::TcpStream;
use tokio::select;

use crate::args::Flags;
use crate::factory::{CliFactory, CliFactoryBuilder};

use crate::worker::CliMainWorker;

deno_core::extension!(cc_deno,
  options = {
      stream_rx:  async_channel::Receiver<TcpStream>
  },
  state = |state, options| {
    state.put(options.stream_rx);
  },
);

pub async fn build_worker(flags: Flags, extensions: Vec<Extension>) -> Result<CliMainWorker, AnyError> {
  // TODO(bartlomieju): actually I think it will also fail if there's an import
  // map specified and bare specifier is used on the command line
  let factory: CliFactory = CliFactory::from_flags(flags).await?;
  let deno_dir = factory.deno_dir()?;
  let http_client = factory.http_client();
  let cli_options = factory.cli_options();
  // Run a background task that checks for available upgrades. If an earlier
  // run of this background task found a new version of Deno.
  super::upgrade::check_for_upgrades(http_client.clone(), deno_dir.upgrade_check_file_path());

  let main_module = cli_options.resolve_main_module()?;

  maybe_npm_install(&factory).await?;
  //开启所有权限
  let permissions = PermissionsContainer::allow_all();
  let worker_factory = factory.create_cli_main_worker_factory().await?;
  let worker = worker_factory
    .create_custom_worker(main_module, permissions, extensions, Default::default())
    .await?;
  Ok(worker)
}

pub async fn run_script(
  flags: Flags,
  stream_rx: async_channel::Receiver<TcpStream>,
  notify_rx: async_channel::Receiver<u8>,
) -> Result<i32, AnyError> {
  // TODO(bartlomieju): actually I think it will also fail if there's an import
  // map specified and bare specifier is used on the command line
  let factory = CliFactory::from_flags(flags).await?;
  let deno_dir = factory.deno_dir()?;
  let http_client = factory.http_client();
  let cli_options = factory.cli_options();
  // Run a background task that checks for available upgrades. If an earlier
  // run of this background task found a new version of Deno.
  super::upgrade::check_for_upgrades(http_client.clone(), deno_dir.upgrade_check_file_path());

  let main_module = cli_options.resolve_main_module()?;

  maybe_npm_install(&factory).await?;
  let permissions = PermissionsContainer::allow_all();
  let worker_factory = factory.create_cli_main_worker_factory().await?;
  let extensions: Vec<_> = vec![cc_deno::init_ops(stream_rx)];
  let mut worker = worker_factory
    .create_custom_worker(main_module, permissions, extensions, Default::default())
    .await?;
  select! {
    _ = notify_rx.recv() => {
        Ok(0)
    },
    _ =  worker.run() => {
         Ok(0)
    }
  }
}

async fn maybe_npm_install(factory: &CliFactory) -> Result<(), AnyError> {
  // ensure an "npm install" is done if the user has explicitly
  // opted into using a node_modules directory
  if factory.cli_options().node_modules_dir_enablement() == Some(true) {
    factory.package_json_deps_installer().await?.ensure_top_level_install().await?;
  }
  Ok(())
}

pub async fn run_with_watch(
  flags: Flags,
  stream_rx: async_channel::Receiver<TcpStream>,
  watch_rx: async_channel::Receiver<bool>,
) -> Result<i32, AnyError> {
  let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
  let factory = CliFactoryBuilder::new().with_watcher(sender.clone()).build_from_flags(flags).await?;
  let file_watcher = factory.file_watcher()?;
  let cli_options = factory.cli_options();
  let clear_screen = !cli_options.no_clear_screen();
  let main_module = cli_options.resolve_main_module()?;
  maybe_npm_install(&factory).await?;
  let create_cli_main_worker_factory = factory.create_cli_main_worker_factory_func().await?;
  let operation = |main_module: ModuleSpecifier| {
    file_watcher.reset();
    let permissions: PermissionsContainer = PermissionsContainer::allow_all();
    let create_cli_main_worker_factory = create_cli_main_worker_factory.clone();
    let extensions: Vec<_> = vec![cc_deno::init_ops(stream_rx.clone())];
    Ok(async move {
      let worker = create_cli_main_worker_factory()
        .create_custom_worker(main_module, permissions, extensions, Default::default())
        .await?;
      worker.run_for_watcher().await?;
      Ok(())
    })
  };

  util::file_watcher::watch_func2(
    receiver,
    operation,
    main_module,
    util::file_watcher::PrintConfig {
      job_name: "Process".to_string(),
      clear_screen,
    },
    watch_rx,
  )
  .await?;

  Ok(0)
}
