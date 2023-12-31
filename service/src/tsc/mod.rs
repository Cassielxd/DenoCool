// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

use crate::args::TsConfig;
use crate::args::TypeCheckMode;
use crate::cache::FastInsecureHasher;
use crate::node;
use crate::util::checksum;
use crate::util::path::mapped_specifier_for_tsc;

use deno_ast::MediaType;
use deno_core::anyhow::anyhow;
use deno_core::anyhow::Context;
use deno_core::ascii_str;
use deno_core::error::AnyError;
use deno_core::located_script_name;
use deno_core::op;
use deno_core::resolve_url_or_path;
use deno_core::serde::Deserialize;
use deno_core::serde::Deserializer;
use deno_core::serde::Serialize;
use deno_core::serde::Serializer;
use deno_core::serde_json;
use deno_core::serde_json::json;
use deno_core::serde_json::Value;
use deno_core::serde_v8;
use deno_core::JsRuntime;
use deno_core::ModuleSpecifier;
use deno_core::OpState;
use deno_core::RuntimeOptions;
use deno_core::Snapshot;
use deno_graph::Module;
use deno_graph::ModuleGraph;
use deno_graph::ResolutionResolved;
use deno_runtime::deno_node;
use deno_runtime::deno_node::NodeResolution;
use deno_runtime::deno_node::NodeResolutionMode;
use deno_runtime::deno_node::NodeResolver;
use deno_runtime::permissions::PermissionsContainer;
use deno_semver::npm::NpmPackageReqReference;
use lsp_types::Url;
use once_cell::sync::Lazy;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

mod diagnostics;

pub use self::diagnostics::Diagnostic;
pub use self::diagnostics::DiagnosticCategory;
pub use self::diagnostics::DiagnosticMessageChain;
pub use self::diagnostics::Diagnostics;
pub use self::diagnostics::Position;

pub static COMPILER_SNAPSHOT: Lazy<Box<[u8]>> = Lazy::new(
  #[cold]
  #[inline(never)]
  || {
    static COMPRESSED_COMPILER_SNAPSHOT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/COMPILER_SNAPSHOT.bin"));

    // NOTE(bartlomieju): Compressing the TSC snapshot in debug build took
    // ~45s on M1 MacBook Pro; without compression it took ~1s.
    // Thus we're not not using compressed snapshot, trading off
    // a lot of build time for some startup time in debug build.
    #[cfg(debug_assertions)]
    return COMPRESSED_COMPILER_SNAPSHOT.to_vec().into_boxed_slice();

    #[cfg(not(debug_assertions))]
    zstd::bulk::decompress(
      &COMPRESSED_COMPILER_SNAPSHOT[4..],
      u32::from_le_bytes(COMPRESSED_COMPILER_SNAPSHOT[0..4].try_into().unwrap()) as usize,
    )
    .unwrap()
    .into_boxed_slice()
  },
);

pub fn get_types_declaration_file_text(unstable: bool) -> String {
  let mut assets = get_asset_texts_from_new_runtime()
    .unwrap()
    .into_iter()
    .map(|a| (a.specifier, a.text))
    .collect::<HashMap<_, _>>();

  let mut lib_names = vec![
    "deno.ns",
    "deno.console",
    "deno.url",
    "deno.web",
    "deno.fetch",
    "deno.websocket",
    "deno.webstorage",
    "deno.crypto",
    "deno.broadcast_channel",
    "deno.net",
    "deno.shared_globals",
    "deno.cache",
    "deno.window",
  ];

  if unstable {
    lib_names.push("deno.unstable");
  }

  lib_names
    .into_iter()
    .map(|name| {
      let asset_url = format!("asset:///lib.{name}.d.ts");
      assets.remove(&asset_url).unwrap()
    })
    .collect::<Vec<_>>()
    .join("\n")
}

fn get_asset_texts_from_new_runtime() -> Result<Vec<AssetText>, AnyError> {
  deno_core::extension!(
    deno_cli_tsc,
    ops_fn = deno_ops,
    customizer = |ext: &mut deno_core::ExtensionBuilder| {
      ext.force_op_registration();
    },
  );

  // the assets are stored within the typescript isolate, so take them out of there
  let mut runtime = JsRuntime::new(RuntimeOptions {
    startup_snapshot: Some(compiler_snapshot()),
    extensions: vec![deno_cli_tsc::init_ops()],
    ..Default::default()
  });
  let global = runtime.execute_script("get_assets.js", ascii_str!("globalThis.getAssets()"))?;
  let scope = &mut runtime.handle_scope();
  let local = deno_core::v8::Local::new(scope, global);
  Ok(serde_v8::from_v8::<Vec<AssetText>>(scope, local)?)
}

pub fn compiler_snapshot() -> Snapshot {
  Snapshot::Static(&COMPILER_SNAPSHOT)
}

macro_rules! inc {
  ($e:expr) => {
    include_str!(concat!("./dts/", $e))
  };
}

/// Contains static assets that are not preloaded in the compiler snapshot.
///
/// We lazily load these because putting them in the compiler snapshot will
/// increase memory usage when not used (last time checked by about 0.5MB).
pub static LAZILY_LOADED_STATIC_ASSETS: Lazy<HashMap<&'static str, &'static str>> = Lazy::new(|| {
  ([
    ("lib.dom.asynciterable.d.ts", inc!("lib.dom.asynciterable.d.ts")),
    ("lib.dom.d.ts", inc!("lib.dom.d.ts")),
    ("lib.dom.extras.d.ts", inc!("lib.dom.extras.d.ts")),
    ("lib.dom.iterable.d.ts", inc!("lib.dom.iterable.d.ts")),
    ("lib.es6.d.ts", inc!("lib.es6.d.ts")),
    ("lib.es2016.full.d.ts", inc!("lib.es2016.full.d.ts")),
    ("lib.es2017.full.d.ts", inc!("lib.es2017.full.d.ts")),
    ("lib.es2018.full.d.ts", inc!("lib.es2018.full.d.ts")),
    ("lib.es2019.full.d.ts", inc!("lib.es2019.full.d.ts")),
    ("lib.es2020.full.d.ts", inc!("lib.es2020.full.d.ts")),
    ("lib.es2021.full.d.ts", inc!("lib.es2021.full.d.ts")),
    ("lib.es2022.full.d.ts", inc!("lib.es2022.full.d.ts")),
    ("lib.esnext.full.d.ts", inc!("lib.esnext.full.d.ts")),
    ("lib.scripthost.d.ts", inc!("lib.scripthost.d.ts")),
    ("lib.webworker.d.ts", inc!("lib.webworker.d.ts")),
    ("lib.webworker.importscripts.d.ts", inc!("lib.webworker.importscripts.d.ts")),
    ("lib.webworker.iterable.d.ts", inc!("lib.webworker.iterable.d.ts")),
    (
      // Special file that can be used to inject the @types/node package.
      // This is used for `node:` specifiers.
      "node_types.d.ts",
      "/// <reference types=\"npm:@types/node\" />\n",
    ),
  ])
  .iter()
  .cloned()
  .collect()
});

/// A structure representing stats from a type check operation for a graph.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Stats(pub Vec<(String, u32)>);

impl<'de> Deserialize<'de> for Stats {
  fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
  where
    D: Deserializer<'de>,
  {
    let items: Vec<(String, u32)> = Deserialize::deserialize(deserializer)?;
    Ok(Stats(items))
  }
}

impl Serialize for Stats {
  fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
  where
    S: Serializer,
  {
    Serialize::serialize(&self.0, serializer)
  }
}

impl fmt::Display for Stats {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    writeln!(f, "Compilation statistics:")?;
    for (key, value) in self.0.clone() {
      writeln!(f, "  {key}: {value}")?;
    }

    Ok(())
  }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetText {
  pub specifier: String,
  pub text: String,
}

/// Retrieve a static asset that are included in the binary.
fn get_lazily_loaded_asset(asset: &str) -> Option<&'static str> {
  LAZILY_LOADED_STATIC_ASSETS.get(asset).map(|s| s.to_owned())
}

fn get_maybe_hash(maybe_source: Option<&str>, hash_data: u64) -> Option<String> {
  maybe_source.map(|source| get_hash(source, hash_data))
}

fn get_hash(source: &str, hash_data: u64) -> String {
  let mut hasher = FastInsecureHasher::new();
  hasher.write_str(source);
  hasher.write_u64(hash_data);
  hasher.finish().to_string()
}

/// Hash the URL so it can be sent to `tsc` in a supportable way
fn hash_url(specifier: &ModuleSpecifier, media_type: MediaType) -> String {
  let hash = checksum::gen(&[specifier.path().as_bytes()]);
  format!("{}:///{}{}", specifier.scheme(), hash, media_type.as_ts_extension())
}

/// If the provided URLs derivable tsc media type doesn't match the media type,
/// we will add an extension to the output.  This is to avoid issues with
/// specifiers that don't have extensions, that tsc refuses to emit because they
/// think a `.js` version exists, when it doesn't.
fn maybe_remap_specifier(specifier: &ModuleSpecifier, media_type: MediaType) -> Option<String> {
  let path = if specifier.scheme() == "file" {
    if let Ok(path) = specifier.to_file_path() {
      path
    } else {
      PathBuf::from(specifier.path())
    }
  } else {
    PathBuf::from(specifier.path())
  };
  if path.extension().is_none() {
    Some(format!("{}{}", specifier, media_type.as_ts_extension()))
  } else {
    None
  }
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct EmittedFile {
  pub data: String,
  pub maybe_specifiers: Option<Vec<ModuleSpecifier>>,
  pub media_type: MediaType,
}

/// A structure representing a request to be sent to the tsc runtime.
#[derive(Debug)]
pub struct Request {
  /// The TypeScript compiler options which will be serialized and sent to
  /// tsc.
  pub config: TsConfig,
  /// Indicates to the tsc runtime if debug logging should occur.
  pub debug: bool,
  pub graph: Arc<ModuleGraph>,
  pub hash_data: u64,
  pub maybe_node_resolver: Option<Arc<NodeResolver>>,
  pub maybe_tsbuildinfo: Option<String>,
  /// A vector of strings that represent the root/entry point modules for the
  /// program.
  pub root_names: Vec<(ModuleSpecifier, MediaType)>,
  pub check_mode: TypeCheckMode,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Response {
  /// Any diagnostics that have been returned from the checker.
  pub diagnostics: Diagnostics,
  /// If there was any build info associated with the exec request.
  pub maybe_tsbuildinfo: Option<String>,
  /// Statistics from the check.
  pub stats: Stats,
}

#[derive(Debug, Default)]
struct State {
  hash_data: u64,
  graph: Arc<ModuleGraph>,
  maybe_tsbuildinfo: Option<String>,
  maybe_response: Option<RespondArgs>,
  maybe_node_resolver: Option<Arc<NodeResolver>>,
  remapped_specifiers: HashMap<String, ModuleSpecifier>,
  root_map: HashMap<String, ModuleSpecifier>,
  current_dir: PathBuf,
}

impl State {
  pub fn new(
    graph: Arc<ModuleGraph>,
    hash_data: u64,
    maybe_node_resolver: Option<Arc<NodeResolver>>,
    maybe_tsbuildinfo: Option<String>,
    root_map: HashMap<String, ModuleSpecifier>,
    remapped_specifiers: HashMap<String, ModuleSpecifier>,
    current_dir: PathBuf,
  ) -> Self {
    State {
      hash_data,
      graph,
      maybe_node_resolver,
      maybe_tsbuildinfo,
      maybe_response: None,
      remapped_specifiers,
      root_map,
      current_dir,
    }
  }
}

fn normalize_specifier(specifier: &str, current_dir: &Path) -> Result<ModuleSpecifier, AnyError> {
  resolve_url_or_path(specifier, current_dir).map_err(|err| err.into())
}

#[op]
fn op_create_hash(s: &mut OpState, text: &str) -> String {
  let state = s.borrow_mut::<State>();
  get_hash(text, state.hash_data)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EmitArgs {
  /// The text data/contents of the file.
  data: String,
  /// The _internal_ filename for the file.  This will be used to determine how
  /// the file is cached and stored.
  file_name: String,
}

#[op]
fn op_emit(state: &mut OpState, args: EmitArgs) -> bool {
  let state = state.borrow_mut::<State>();
  match args.file_name.as_ref() {
    "internal:///.tsbuildinfo" => state.maybe_tsbuildinfo = Some(args.data),
    _ => {
      if cfg!(debug_assertions) {
        panic!("Unhandled emit write: {}", args.file_name);
      }
    }
  }

  true
}

#[derive(Debug, Deserialize)]
struct LoadArgs {
  /// The fully qualified specifier that should be loaded.
  specifier: String,
}

pub fn as_ts_script_kind(media_type: MediaType) -> i32 {
  match media_type {
    MediaType::JavaScript => 1,
    MediaType::Jsx => 2,
    MediaType::Mjs => 1,
    MediaType::Cjs => 1,
    MediaType::TypeScript => 3,
    MediaType::Mts => 3,
    MediaType::Cts => 3,
    MediaType::Dts => 3,
    MediaType::Dmts => 3,
    MediaType::Dcts => 3,
    MediaType::Tsx => 4,
    MediaType::Json => 6,
    MediaType::SourceMap | MediaType::TsBuildInfo | MediaType::Wasm | MediaType::Unknown => 0,
  }
}

#[op]
fn op_load(state: &mut OpState, args: Value) -> Result<Value, AnyError> {
  let state = state.borrow_mut::<State>();
  let v: LoadArgs = serde_json::from_value(args).context("Invalid request from JavaScript for \"op_load\".")?;
  let specifier = normalize_specifier(&v.specifier, &state.current_dir).context("Error converting a string module specifier for \"op_load\".")?;
  let mut hash: Option<String> = None;
  let mut media_type = MediaType::Unknown;
  let graph = &state.graph;
  let data = if &v.specifier == "internal:///.tsbuildinfo" {
    state.maybe_tsbuildinfo.as_deref().map(Cow::Borrowed)
  // in certain situations we return a "blank" module to tsc and we need to
  // handle the request for that module here.
  } else if &v.specifier == "internal:///missing_dependency.d.ts" {
    hash = Some("1".to_string());
    media_type = MediaType::Dts;
    Some(Cow::Borrowed("declare const __: any;\nexport = __;\n"))
  } else if let Some(name) = v.specifier.strip_prefix("asset:///") {
    let maybe_source = get_lazily_loaded_asset(name);
    hash = get_maybe_hash(maybe_source, state.hash_data);
    media_type = MediaType::from_str(&v.specifier);
    maybe_source.map(Cow::Borrowed)
  } else {
    let specifier = if let Some(remapped_specifier) = state.remapped_specifiers.get(&v.specifier) {
      remapped_specifier
    } else if let Some(remapped_specifier) = state.root_map.get(&v.specifier) {
      remapped_specifier
    } else {
      &specifier
    };
    let maybe_source = if let Some(module) = graph.get(specifier) {
      match module {
        Module::Esm(module) => {
          media_type = module.media_type;
          Some(Cow::Borrowed(&*module.source))
        }
        Module::Json(module) => {
          media_type = MediaType::Json;
          Some(Cow::Borrowed(&*module.source))
        }
        Module::Npm(_) | Module::Node(_) => None,
        Module::External(module) => {
          // means it's Deno code importing an npm module
          let specifier = node::resolve_specifier_into_node_modules(&module.specifier);
          media_type = MediaType::from_specifier(&specifier);
          let file_path = specifier.to_file_path().unwrap();
          let code = std::fs::read_to_string(&file_path).with_context(|| format!("Unable to load {}", file_path.display()))?;
          Some(Cow::Owned(code))
        }
      }
    } else if state
      .maybe_node_resolver
      .as_ref()
      .map(|resolver| resolver.in_npm_package(specifier))
      .unwrap_or(false)
    {
      media_type = MediaType::from_specifier(specifier);
      let file_path = specifier.to_file_path().unwrap();
      let code = std::fs::read_to_string(&file_path).with_context(|| format!("Unable to load {}", file_path.display()))?;
      Some(Cow::Owned(code))
    } else {
      media_type = MediaType::Unknown;
      None
    };
    hash = get_maybe_hash(maybe_source.as_deref(), state.hash_data);
    maybe_source
  };

  Ok(json!({
    "data": data,
    "version": hash,
    "scriptKind": as_ts_script_kind(media_type),
  }))
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveArgs {
  /// The base specifier that the supplied specifier strings should be resolved
  /// relative to.
  pub base: String,
  /// A list of specifiers that should be resolved.
  pub specifiers: Vec<String>,
}

#[op]
fn op_resolve(state: &mut OpState, args: ResolveArgs) -> Result<Vec<(String, String)>, AnyError> {
  let state = state.borrow_mut::<State>();
  let mut resolved: Vec<(String, String)> = Vec::with_capacity(args.specifiers.len());
  let referrer = if let Some(remapped_specifier) = state.remapped_specifiers.get(&args.base) {
    remapped_specifier.clone()
  } else if let Some(remapped_base) = state.root_map.get(&args.base) {
    remapped_base.clone()
  } else {
    normalize_specifier(&args.base, &state.current_dir).context("Error converting a string module specifier for \"op_resolve\".")?
  };
  for specifier in args.specifiers {
    if let Some(module_name) = specifier.strip_prefix("node:") {
      if deno_node::is_builtin_node_module(module_name) {
        // return itself for node: specifiers because during type checking
        // we resolve to the ambient modules in the @types/node package
        // rather than deno_std/node
        resolved.push((specifier, MediaType::Dts.to_string()));
        continue;
      }
    }

    if specifier.starts_with("asset:///") {
      let media_type = MediaType::from_str(&specifier).as_ts_extension().to_string();
      resolved.push((specifier, media_type));
      continue;
    }

    let graph = &state.graph;
    let resolved_dep = graph
      .get(&referrer)
      .and_then(|m| m.esm())
      .and_then(|m| m.dependencies.get(&specifier))
      .and_then(|d| d.maybe_type.ok().or_else(|| d.maybe_code.ok()));

    let maybe_result = match resolved_dep {
      Some(ResolutionResolved { specifier, .. }) => resolve_graph_specifier_types(specifier, state)?,
      _ => resolve_non_graph_specifier_types(&specifier, &referrer, state)?,
    };
    let result = match maybe_result {
      Some((specifier, media_type)) => {
        let specifier_str = match specifier.scheme() {
          "data" | "blob" => {
            let specifier_str = hash_url(&specifier, media_type);
            state.remapped_specifiers.insert(specifier_str.clone(), specifier);
            specifier_str
          }
          _ => {
            if let Some(specifier_str) = maybe_remap_specifier(&specifier, media_type) {
              state.remapped_specifiers.insert(specifier_str.clone(), specifier);
              specifier_str
            } else {
              specifier.to_string()
            }
          }
        };
        (specifier_str, media_type.as_ts_extension().into())
      }
      None => ("internal:///missing_dependency.d.ts".to_string(), ".d.ts".to_string()),
    };
    log::debug!("Resolved {} to {:?}", specifier, result);
    resolved.push(result);
  }

  Ok(resolved)
}

fn resolve_graph_specifier_types(specifier: &ModuleSpecifier, state: &State) -> Result<Option<(ModuleSpecifier, MediaType)>, AnyError> {
  let graph = &state.graph;
  let maybe_module = graph.get(specifier);
  // follow the types reference directive, which may be pointing at an npm package
  let maybe_module = match maybe_module {
    Some(Module::Esm(module)) => {
      let maybe_types_dep = module.maybe_types_dependency.as_ref().map(|d| &d.dependency);
      match maybe_types_dep.and_then(|d| d.maybe_specifier()) {
        Some(specifier) => graph.get(specifier),
        _ => maybe_module,
      }
    }
    maybe_module => maybe_module,
  };

  // now get the types from the resolved module
  match maybe_module {
    Some(Module::Esm(module)) => Ok(Some((module.specifier.clone(), module.media_type))),
    Some(Module::Json(module)) => Ok(Some((module.specifier.clone(), module.media_type))),
    Some(Module::Npm(module)) => {
      if let Some(node_resolver) = &state.maybe_node_resolver {
        let maybe_resolution =
          node_resolver.resolve_npm_reference(&module.nv_reference, NodeResolutionMode::Types, &PermissionsContainer::allow_all())?;
        Ok(Some(NodeResolution::into_specifier_and_media_type(maybe_resolution)))
      } else {
        Ok(None)
      }
    }
    Some(Module::External(module)) => {
      // we currently only use "External" for when the module is in an npm package
      Ok(state.maybe_node_resolver.as_ref().map(|node_resolver| {
        let specifier = node::resolve_specifier_into_node_modules(&module.specifier);
        NodeResolution::into_specifier_and_media_type(node_resolver.url_to_node_resolution(specifier).ok())
      }))
    }
    Some(Module::Node(_)) | None => Ok(None),
  }
}

fn resolve_non_graph_specifier_types(
  specifier: &str,
  referrer: &ModuleSpecifier,
  state: &State,
) -> Result<Option<(ModuleSpecifier, MediaType)>, AnyError> {
  let node_resolver = match state.maybe_node_resolver.as_ref() {
    Some(node_resolver) => node_resolver,
    None => return Ok(None), // we only support non-graph types for npm packages
  };
  if node_resolver.in_npm_package(referrer) {
    // we're in an npm package, so use node resolution
    Ok(Some(NodeResolution::into_specifier_and_media_type(
      node_resolver
        .resolve(specifier, referrer, NodeResolutionMode::Types, &PermissionsContainer::allow_all())
        .ok()
        .flatten(),
    )))
  } else if let Ok(npm_ref) = NpmPackageReqReference::from_str(specifier) {
    // todo(dsherret): add support for injecting this in the graph so
    // we don't need this special code here.
    // This could occur when resolving npm:@types/node when it is
    // injected and not part of the graph
    let maybe_resolution = node_resolver.resolve_npm_req_reference(&npm_ref, NodeResolutionMode::Types, &PermissionsContainer::allow_all())?;
    Ok(Some(NodeResolution::into_specifier_and_media_type(maybe_resolution)))
  } else {
    Ok(None)
  }
}

#[op]
fn op_is_node_file(state: &mut OpState, path: &str) -> bool {
  let state = state.borrow::<State>();
  match ModuleSpecifier::parse(path) {
    Ok(specifier) => state.maybe_node_resolver.as_ref().map(|r| r.in_npm_package(&specifier)).unwrap_or(false),
    Err(_) => false,
  }
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
struct RespondArgs {
  pub diagnostics: Diagnostics,
  pub stats: Stats,
}

#[op]
fn op_respond(state: &mut OpState, args: Value) -> Result<Value, AnyError> {
  let state = state.borrow_mut::<State>();
  let v: RespondArgs = serde_json::from_value(args).context("Error converting the result for \"op_respond\".")?;
  state.maybe_response = Some(v);
  Ok(json!(true))
}

/// Execute a request on the supplied snapshot, returning a response which
/// contains information, like any emitted files, diagnostics, statistics and
/// optionally an updated TypeScript build info.
pub fn exec(request: Request) -> Result<Response, AnyError> {
  // tsc cannot handle root specifiers that don't have one of the "acceptable"
  // extensions.  Therefore, we have to check the root modules against their
  // extensions and remap any that are unacceptable to tsc and add them to the
  // op state so when requested, we can remap to the original specifier.
  let mut root_map = HashMap::new();
  let mut remapped_specifiers = HashMap::new();
  let root_names: Vec<String> = request
    .root_names
    .iter()
    .map(|(s, mt)| match s.scheme() {
      "data" | "blob" => {
        let specifier_str = hash_url(s, *mt);
        remapped_specifiers.insert(specifier_str.clone(), s.clone());
        specifier_str
      }
      _ => {
        if let Some(new_specifier) = mapped_specifier_for_tsc(s, *mt) {
          root_map.insert(new_specifier.clone(), s.clone());
          new_specifier
        } else {
          s.to_string()
        }
      }
    })
    .collect();

  deno_core::extension!(deno_cli_tsc,
    ops_fn = deno_ops,
    options = {
      request: Request,
      root_map: HashMap<String, Url>,
      remapped_specifiers: HashMap<String, Url>,
    },
    state = |state, options| {
      state.put(State::new(
        options.request.graph,
        options.request.hash_data,
        options.request.maybe_node_resolver,
        options.request.maybe_tsbuildinfo,
        options.root_map,
        options.remapped_specifiers,
        std::env::current_dir()
          .context("Unable to get CWD")
          .unwrap(),
      ));
    },
    customizer = |ext: &mut deno_core::ExtensionBuilder| {
      ext.force_op_registration();
    },
  );

  let startup_source = ascii_str!("globalThis.startup({ legacyFlag: false })");
  let request_value = json!({
    "config": request.config,
    "debug": request.debug,
    "rootNames": root_names,
    "localOnly": request.check_mode == TypeCheckMode::Local,
  });
  let exec_source = format!("globalThis.exec({request_value})").into();

  let mut runtime = JsRuntime::new(RuntimeOptions {
    startup_snapshot: Some(compiler_snapshot()),
    extensions: vec![deno_cli_tsc::init_ops(request, root_map, remapped_specifiers)],
    ..Default::default()
  });

  runtime
    .execute_script(located_script_name!(), startup_source)
    .context("Could not properly start the compiler runtime.")?;
  runtime.execute_script(located_script_name!(), exec_source)?;

  let op_state = runtime.op_state();
  let mut op_state = op_state.borrow_mut();
  let state = op_state.take::<State>();

  if let Some(response) = state.maybe_response {
    let diagnostics = response.diagnostics;
    let maybe_tsbuildinfo = state.maybe_tsbuildinfo;
    let stats = response.stats;

    Ok(Response {
      diagnostics,
      maybe_tsbuildinfo,
      stats,
    })
  } else {
    Err(anyhow!("The response for the exec request was not set."))
  }
}

deno_core::ops!(deno_ops, [op_create_hash, op_emit, op_is_node_file, op_load, op_resolve, op_respond,]);
