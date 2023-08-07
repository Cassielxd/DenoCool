// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use deno_ast::ModuleSpecifier;
use deno_core::anyhow::Context;
use deno_core::error::AnyError;
use deno_core::futures::task::LocalFutureObj;
use deno_core::futures::FutureExt;
use deno_core::located_script_name;
use deno_core::parking_lot::Mutex;
use deno_core::url::Url;
use deno_core::CompiledWasmModuleStore;
use deno_core::Extension;
use deno_core::ModuleId;
use deno_core::ModuleLoader;
use deno_core::SharedArrayBufferStore;
use deno_core::SourceMapGetter;
use deno_lockfile::Lockfile;
use deno_runtime::colors;
use deno_runtime::deno_broadcast_channel::InMemoryBroadcastChannel;
use deno_runtime::deno_fs;
use deno_runtime::deno_node;
use deno_runtime::deno_node::NodeResolution;
use deno_runtime::deno_node::NodeResolver;
use deno_runtime::deno_tls::RootCertStoreProvider;
use deno_runtime::deno_web::BlobStore;
use deno_runtime::fmt_errors::format_js_error;
use deno_runtime::inspector_server::InspectorServer;
use deno_runtime::ops::worker_host::CreateWebWorkerCb;
use deno_runtime::ops::worker_host::WorkerEventCb;
use deno_runtime::permissions::PermissionsContainer;
use deno_runtime::web_worker::WebWorker;
use deno_runtime::web_worker::WebWorkerOptions;
use deno_runtime::worker::MainWorker;
use deno_runtime::worker::WorkerOptions;
use deno_runtime::BootstrapOptions;
use deno_semver::npm::NpmPackageReqReference;

use crate::args::StorageKeyResolver;
use crate::errors;
use crate::npm::CliNpmResolver;
use crate::ops;
use crate::tools;
use crate::tools::coverage::CoverageCollector;
use crate::util::checksum;
use crate::version;

pub trait ModuleLoaderFactory: Send + Sync {
  fn create_for_main(&self, root_permissions: PermissionsContainer, dynamic_permissions: PermissionsContainer) -> Rc<dyn ModuleLoader>;

  fn create_for_worker(&self, root_permissions: PermissionsContainer, dynamic_permissions: PermissionsContainer) -> Rc<dyn ModuleLoader>;

  fn create_source_map_getter(&self) -> Option<Box<dyn SourceMapGetter>>;
}

// todo(dsherret): this is temporary and we should remove this
// once we no longer conditionally initialize the node runtime
pub trait HasNodeSpecifierChecker: Send + Sync {
  fn has_node_specifier(&self) -> bool;
}

#[derive(Clone)]
pub struct CliMainWorkerOptions {
  pub argv: Vec<String>,
  pub debug: bool,
  pub coverage_dir: Option<String>,
  pub enable_testing_features: bool,
  pub has_node_modules_dir: bool,
  pub inspect_brk: bool,
  pub inspect_wait: bool,
  pub is_inspecting: bool,
  pub is_npm_main: bool,
  pub location: Option<Url>,
  pub maybe_binary_npm_command_name: Option<String>,
  pub origin_data_folder_path: Option<PathBuf>,
  pub seed: Option<u64>,
  pub unsafely_ignore_certificate_errors: Option<Vec<String>>,
  pub unstable: bool,
}

struct SharedWorkerState {
  options: CliMainWorkerOptions,
  storage_key_resolver: StorageKeyResolver,
  npm_resolver: Arc<CliNpmResolver>,
  node_resolver: Arc<NodeResolver>,
  has_node_specifier_checker: Box<dyn HasNodeSpecifierChecker>,
  blob_store: BlobStore,
  broadcast_channel: InMemoryBroadcastChannel,
  shared_array_buffer_store: SharedArrayBufferStore,
  compiled_wasm_module_store: CompiledWasmModuleStore,
  module_loader_factory: Box<dyn ModuleLoaderFactory>,
  root_cert_store_provider: Arc<dyn RootCertStoreProvider>,
  fs: Arc<dyn deno_fs::FileSystem>,
  maybe_inspector_server: Option<Arc<InspectorServer>>,
  maybe_lockfile: Option<Arc<Mutex<Lockfile>>>,
}

impl SharedWorkerState {
  pub fn should_initialize_node_runtime(&self) -> bool {
    self.npm_resolver.has_packages() || self.has_node_specifier_checker.has_node_specifier() || self.options.is_npm_main
  }
}

pub struct CliMainWorker {
  main_module: ModuleSpecifier,
  is_main_cjs: bool,
  pub worker: MainWorker,
  shared: Arc<SharedWorkerState>,
}

impl CliMainWorker {
  pub fn into_main_worker(self) -> MainWorker {
    self.worker
  }

  pub async fn setup_repl(&mut self) -> Result<(), AnyError> {
    self.worker.run_event_loop(false).await?;
    Ok(())
  }

  pub async fn run(&mut self) -> Result<i32, AnyError> {
    let mut maybe_coverage_collector: Option<CoverageCollector> = self.maybe_setup_coverage_collector().await?;
    log::debug!("main_module {}", self.main_module);
    println!("{}", self.main_module);
    if self.is_main_cjs {
      self.initialize_main_module_for_node()?;
      deno_node::load_cjs_module(
        &mut self.worker.js_runtime,
        &self.main_module.to_file_path().unwrap().to_string_lossy(),
        true,
        self.shared.options.inspect_brk,
      )?;
    } else {
      self.execute_main_module_possibly_with_npm().await?;
    }

    self.worker.dispatch_load_event(located_script_name!())?;

    loop {
      self.worker.run_event_loop(maybe_coverage_collector.is_none()).await?;
      if !self.worker.dispatch_beforeunload_event(located_script_name!())? {
        break;
      }
    }

    self.worker.dispatch_unload_event(located_script_name!())?;

    if let Some(coverage_collector) = maybe_coverage_collector.as_mut() {
      self.worker.with_event_loop(coverage_collector.stop_collecting().boxed_local()).await?;
    }

    Ok(self.worker.exit_code())
  }

  pub async fn run_for_watcher(self) -> Result<(), AnyError> {
    /// The FileWatcherModuleExecutor provides module execution with safe dispatching of life-cycle events by tracking the
    /// state of any pending events and emitting accordingly on drop in the case of a future
    /// cancellation.
    struct FileWatcherModuleExecutor {
      inner: CliMainWorker,
      pending_unload: bool,
    }

    impl FileWatcherModuleExecutor {
      pub fn new(worker: CliMainWorker) -> FileWatcherModuleExecutor {
        FileWatcherModuleExecutor {
          inner: worker,
          pending_unload: false,
        }
      }

      /// Execute the given main module emitting load and unload events before and after execution
      /// respectively.
      pub async fn execute(&mut self) -> Result<(), AnyError> {
        self.inner.execute_main_module_possibly_with_npm().await?;
        self.inner.worker.dispatch_load_event(located_script_name!())?;
        self.pending_unload = true;

        let result = loop {
          match self.inner.worker.run_event_loop(false).await {
            Ok(()) => {}
            Err(error) => break Err(error),
          }
          match self.inner.worker.dispatch_beforeunload_event(located_script_name!()) {
            Ok(default_prevented) if default_prevented => {} // continue loop
            Ok(_) => break Ok(()),
            Err(error) => break Err(error),
          }
        };
        self.pending_unload = false;

        result?;

        self.inner.worker.dispatch_unload_event(located_script_name!())?;

        Ok(())
      }
    }

    impl Drop for FileWatcherModuleExecutor {
      fn drop(&mut self) {
        if self.pending_unload {
          let _ = self.inner.worker.dispatch_unload_event(located_script_name!());
        }
      }
    }

    let mut executor = FileWatcherModuleExecutor::new(self);
    executor.execute().await
  }

  pub async fn execute_main_module_possibly_with_npm(&mut self) -> Result<(), AnyError> {
    let id = self.worker.preload_main_module(&self.main_module).await?;
    self.evaluate_module_possibly_with_npm(id).await
  }

  pub async fn execute_side_module_possibly_with_npm(&mut self) -> Result<(), AnyError> {
    let id = self.worker.preload_side_module(&self.main_module).await?;
    self.evaluate_module_possibly_with_npm(id).await
  }

  async fn evaluate_module_possibly_with_npm(&mut self, id: ModuleId) -> Result<(), AnyError> {
    if self.shared.should_initialize_node_runtime() {
      self.initialize_main_module_for_node()?;
    }
    self.worker.evaluate_module(id).await
  }

  fn initialize_main_module_for_node(&mut self) -> Result<(), AnyError> {
    deno_node::initialize_runtime(
      &mut self.worker.js_runtime,
      self.shared.options.has_node_modules_dir,
      self.shared.options.maybe_binary_npm_command_name.as_deref(),
    )?;

    Ok(())
  }

  pub async fn maybe_setup_coverage_collector(&mut self) -> Result<Option<CoverageCollector>, AnyError> {
    if let Some(coverage_dir) = &self.shared.options.coverage_dir {
      let session = self.worker.create_inspector_session().await;

      let coverage_dir = PathBuf::from(coverage_dir);
      let mut coverage_collector = tools::coverage::CoverageCollector::new(coverage_dir, session);
      self.worker.with_event_loop(coverage_collector.start_collecting().boxed_local()).await?;
      Ok(Some(coverage_collector))
    } else {
      Ok(None)
    }
  }
}

pub struct CliMainWorkerFactory {
  shared: Arc<SharedWorkerState>,
}

impl CliMainWorkerFactory {
  #[allow(clippy::too_many_arguments)]
  pub fn new(
    storage_key_resolver: StorageKeyResolver,
    npm_resolver: Arc<CliNpmResolver>,
    node_resolver: Arc<NodeResolver>,
    has_node_specifier_checker: Box<dyn HasNodeSpecifierChecker>,
    blob_store: BlobStore,
    module_loader_factory: Box<dyn ModuleLoaderFactory>,
    root_cert_store_provider: Arc<dyn RootCertStoreProvider>,
    fs: Arc<dyn deno_fs::FileSystem>,
    maybe_inspector_server: Option<Arc<InspectorServer>>,
    maybe_lockfile: Option<Arc<Mutex<Lockfile>>>,
    options: CliMainWorkerOptions,
  ) -> Self {
    Self {
      shared: Arc::new(SharedWorkerState {
        options,
        storage_key_resolver,
        npm_resolver,
        node_resolver,
        has_node_specifier_checker,
        blob_store,
        broadcast_channel: Default::default(),
        shared_array_buffer_store: Default::default(),
        compiled_wasm_module_store: Default::default(),
        module_loader_factory,
        root_cert_store_provider,
        fs,
        maybe_inspector_server,
        maybe_lockfile,
      }),
    }
  }

  pub async fn create_main_worker(&self, main_module: ModuleSpecifier, permissions: PermissionsContainer) -> Result<CliMainWorker, AnyError> {
    self.create_custom_worker(main_module, permissions, vec![], Default::default()).await
  }

  pub async fn create_custom_worker(
    &self,
    main_module: ModuleSpecifier,
    permissions: PermissionsContainer,
    mut custom_extensions: Vec<Extension>,
    stdio: deno_runtime::deno_io::Stdio,
  ) -> Result<CliMainWorker, AnyError> {
    let shared = &self.shared;
    let (main_module, is_main_cjs) = if let Ok(package_ref) = NpmPackageReqReference::from_specifier(&main_module) {
      shared.npm_resolver.add_package_reqs(&[package_ref.req.clone()]).await?;
      let node_resolution = shared.node_resolver.resolve_binary_export(&package_ref)?;
      let is_main_cjs = matches!(node_resolution, NodeResolution::CommonJs(_));

      if let Some(lockfile) = &shared.maybe_lockfile {
        // For npm binary commands, ensure that the lockfile gets updated
        // so that we can re-use the npm resolution the next time it runs
        // for better performance
        lockfile.lock().write().context("Failed writing lockfile.")?;
      }

      (node_resolution.into_url(), is_main_cjs)
    } else if shared.options.is_npm_main {
      let node_resolution = shared.node_resolver.url_to_node_resolution(main_module)?;
      let is_main_cjs = matches!(node_resolution, NodeResolution::CommonJs(_));
      (node_resolution.into_url(), is_main_cjs)
    } else {
      (main_module, false)
    };

    let module_loader = shared
      .module_loader_factory
      .create_for_main(PermissionsContainer::allow_all(), permissions.clone());
    let maybe_source_map_getter = shared.module_loader_factory.create_source_map_getter();
    let maybe_inspector_server = shared.maybe_inspector_server.clone();

    let create_web_worker_cb = create_web_worker_callback(shared.clone(), stdio.clone());
    let web_worker_preload_module_cb = create_web_worker_preload_module_callback(shared);
    let web_worker_pre_execute_module_cb = create_web_worker_pre_execute_module_callback(shared.clone());

    let maybe_storage_key = shared.storage_key_resolver.resolve_storage_key(&main_module);
    let origin_storage_dir = maybe_storage_key.as_ref().map(|key| {
      shared
        .options
        .origin_data_folder_path
        .as_ref()
        .unwrap() // must be set if storage key resolver returns a value
        .join(checksum::gen(&[key.as_bytes()]))
    });
    let cache_storage_dir = maybe_storage_key.map(|key| {
      // TODO(@satyarohith): storage quota management
      // Note: we currently use temp_dir() to avoid managing storage size.
      std::env::temp_dir().join("deno_cache").join(checksum::gen(&[key.as_bytes()]))
    });

    let mut extensions = ops::cli_exts(shared.npm_resolver.clone());
    extensions.append(&mut custom_extensions);

    let options = WorkerOptions {
      bootstrap: BootstrapOptions {
        args: shared.options.argv.clone(),
        cpu_count: std::thread::available_parallelism().map(|p| p.get()).unwrap_or(1),
        debug_flag: shared.options.debug,
        enable_testing_features: shared.options.enable_testing_features,
        locale: deno_core::v8::icu::get_language_tag(),
        location: shared.options.location.clone(),
        no_color: !colors::use_color(),
        is_tty: colors::is_tty(),
        runtime_version: version::deno().to_string(),
        ts_version: version::TYPESCRIPT.to_string(),
        unstable: shared.options.unstable,
        user_agent: version::get_user_agent().to_string(),
        inspect: shared.options.is_inspecting,
      },
      extensions,
      startup_snapshot: Some(crate::js::deno_isolate_init()),
      unsafely_ignore_certificate_errors: shared.options.unsafely_ignore_certificate_errors.clone(),
      root_cert_store_provider: Some(shared.root_cert_store_provider.clone()),
      seed: shared.options.seed,
      source_map_getter: maybe_source_map_getter,
      format_js_error_fn: Some(Arc::new(format_js_error)),
      create_web_worker_cb,
      web_worker_preload_module_cb,
      web_worker_pre_execute_module_cb,
      maybe_inspector_server,
      should_break_on_first_statement: shared.options.inspect_brk,
      should_wait_for_inspector_session: shared.options.inspect_wait,
      module_loader,
      fs: shared.fs.clone(),
      npm_resolver: Some(shared.npm_resolver.clone()),
      get_error_class_fn: Some(&errors::get_error_class_name),
      cache_storage_dir,
      origin_storage_dir,
      blob_store: shared.blob_store.clone(),
      broadcast_channel: shared.broadcast_channel.clone(),
      shared_array_buffer_store: Some(shared.shared_array_buffer_store.clone()),
      compiled_wasm_module_store: Some(shared.compiled_wasm_module_store.clone()),
      stdio,
    };
    let worker = MainWorker::bootstrap_from_options(main_module.clone(), permissions, options);

    Ok(CliMainWorker {
      main_module,
      is_main_cjs,
      worker,
      shared: shared.clone(),
    })
  }
}

// TODO(bartlomieju): this callback could have default value
// and not be required
fn create_web_worker_preload_module_callback(_shared: &Arc<SharedWorkerState>) -> Arc<WorkerEventCb> {
  Arc::new(move |worker| {
    let fut = async move { Ok(worker) };
    LocalFutureObj::new(Box::new(fut))
  })
}

fn create_web_worker_pre_execute_module_callback(shared: Arc<SharedWorkerState>) -> Arc<WorkerEventCb> {
  Arc::new(move |mut worker| {
    let shared = shared.clone();
    let fut = async move {
      // this will be up to date after pre-load
      if shared.should_initialize_node_runtime() {
        deno_node::initialize_runtime(&mut worker.js_runtime, shared.options.has_node_modules_dir, None)?;
      }

      Ok(worker)
    };
    LocalFutureObj::new(Box::new(fut))
  })
}

fn create_web_worker_callback(shared: Arc<SharedWorkerState>, stdio: deno_runtime::deno_io::Stdio) -> Arc<CreateWebWorkerCb> {
  Arc::new(move |args| {
    let maybe_inspector_server = shared.maybe_inspector_server.clone();

    let module_loader = shared
      .module_loader_factory
      .create_for_worker(args.parent_permissions.clone(), args.permissions.clone());
    let maybe_source_map_getter = shared.module_loader_factory.create_source_map_getter();
    let create_web_worker_cb = create_web_worker_callback(shared.clone(), stdio.clone());
    let preload_module_cb = create_web_worker_preload_module_callback(&shared);
    let pre_execute_module_cb = create_web_worker_pre_execute_module_callback(shared.clone());

    let extensions = ops::cli_exts(shared.npm_resolver.clone());

    let maybe_storage_key = shared.storage_key_resolver.resolve_storage_key(&args.main_module);
    let cache_storage_dir = maybe_storage_key.map(|key| {
      // TODO(@satyarohith): storage quota management
      // Note: we currently use temp_dir() to avoid managing storage size.
      std::env::temp_dir().join("deno_cache").join(checksum::gen(&[key.as_bytes()]))
    });

    let options = WebWorkerOptions {
      bootstrap: BootstrapOptions {
        args: shared.options.argv.clone(),
        cpu_count: std::thread::available_parallelism().map(|p| p.get()).unwrap_or(1),
        debug_flag: shared.options.debug,
        enable_testing_features: shared.options.enable_testing_features,
        locale: deno_core::v8::icu::get_language_tag(),
        location: Some(args.main_module.clone()),
        no_color: !colors::use_color(),
        is_tty: colors::is_tty(),
        runtime_version: version::deno().to_string(),
        ts_version: version::TYPESCRIPT.to_string(),
        unstable: shared.options.unstable,
        user_agent: version::get_user_agent().to_string(),
        inspect: shared.options.is_inspecting,
      },
      extensions,
      startup_snapshot: Some(crate::js::deno_isolate_init()),
      unsafely_ignore_certificate_errors: shared.options.unsafely_ignore_certificate_errors.clone(),
      root_cert_store_provider: Some(shared.root_cert_store_provider.clone()),
      seed: shared.options.seed,
      create_web_worker_cb,
      preload_module_cb,
      pre_execute_module_cb,
      format_js_error_fn: Some(Arc::new(format_js_error)),
      source_map_getter: maybe_source_map_getter,
      module_loader,
      fs: shared.fs.clone(),
      npm_resolver: Some(shared.npm_resolver.clone()),
      worker_type: args.worker_type,
      maybe_inspector_server,
      get_error_class_fn: Some(&errors::get_error_class_name),
      blob_store: shared.blob_store.clone(),
      broadcast_channel: shared.broadcast_channel.clone(),
      shared_array_buffer_store: Some(shared.shared_array_buffer_store.clone()),
      compiled_wasm_module_store: Some(shared.compiled_wasm_module_store.clone()),
      stdio: stdio.clone(),
      cache_storage_dir,
    };

    WebWorker::bootstrap_from_options(args.name, args.permissions, args.main_module, args.worker_id, options)
  })
}
