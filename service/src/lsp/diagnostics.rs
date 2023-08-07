// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

use super::analysis;
use super::cache;
use super::client::Client;
use super::config::ConfigSnapshot;
use super::documents;
use super::documents::Document;
use super::documents::DocumentsFilter;
use super::language_server;
use super::language_server::StateSnapshot;
use super::performance::Performance;
use super::tsc;
use super::tsc::TsServer;

use crate::args::LintOptions;
use crate::graph_util;
use crate::graph_util::enhanced_resolution_error_message;
use crate::lsp::lsp_custom::DiagnosticBatchNotificationParams;
use crate::tools::lint::get_configured_rules;

use deno_ast::MediaType;
use deno_core::anyhow::anyhow;
use deno_core::error::AnyError;
use deno_core::resolve_url;
use deno_core::serde::Deserialize;
use deno_core::serde_json;
use deno_core::serde_json::json;
use deno_core::task::spawn;
use deno_core::task::JoinHandle;
use deno_core::ModuleSpecifier;
use deno_graph::Resolution;
use deno_graph::ResolutionError;
use deno_graph::SpecifierError;
use deno_lint::rules::LintRule;
use deno_runtime::deno_node;
use deno_runtime::tokio_util::create_basic_runtime;
use deno_semver::npm::NpmPackageReqReference;
use log::error;
use std::collections::HashMap;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::thread;
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;
use tower_lsp::lsp_types as lsp;

#[derive(Debug)]
pub struct DiagnosticServerUpdateMessage {
  pub snapshot: Arc<StateSnapshot>,
  pub config: Arc<ConfigSnapshot>,
  pub lint_options: LintOptions,
}

pub type DiagnosticRecord = (ModuleSpecifier, Option<i32>, Vec<lsp::Diagnostic>);
pub type DiagnosticVec = Vec<DiagnosticRecord>;
type DiagnosticMap = HashMap<ModuleSpecifier, (Option<i32>, Vec<lsp::Diagnostic>)>;
type DiagnosticsByVersionMap = HashMap<Option<i32>, Vec<lsp::Diagnostic>>;

#[derive(Clone)]
struct DiagnosticsPublisher {
  client: Client,
  all_diagnostics: Arc<Mutex<HashMap<ModuleSpecifier, DiagnosticsByVersionMap>>>,
}

impl DiagnosticsPublisher {
  pub fn new(client: Client) -> Self {
    Self {
      client,
      all_diagnostics: Default::default(),
    }
  }

  pub async fn publish(&self, diagnostics: DiagnosticVec, token: &CancellationToken) {
    let mut all_diagnostics = self.all_diagnostics.lock().await;
    for (specifier, version, diagnostics) in diagnostics {
      if token.is_cancelled() {
        return;
      }

      // the versions of all the published diagnostics should be the same, but just
      // in case they're not keep track of that
      let diagnostics_by_version = all_diagnostics.entry(specifier.clone()).or_default();
      let version_diagnostics = diagnostics_by_version.entry(version).or_default();
      version_diagnostics.extend(diagnostics);

      self
        .client
        .when_outside_lsp_lock()
        .publish_diagnostics(specifier, version_diagnostics.clone(), version)
        .await;
    }
  }

  pub async fn clear(&self) {
    let mut all_diagnostics = self.all_diagnostics.lock().await;
    all_diagnostics.clear();
  }
}

#[derive(Clone, Default, Debug)]
struct TsDiagnosticsStore(Arc<deno_core::parking_lot::Mutex<DiagnosticMap>>);

impl TsDiagnosticsStore {
  pub fn get(&self, specifier: &ModuleSpecifier, document_version: Option<i32>) -> Vec<lsp::Diagnostic> {
    let ts_diagnostics = self.0.lock();
    if let Some((diagnostics_doc_version, diagnostics)) = ts_diagnostics.get(specifier) {
      // only get the diagnostics if they're up to date
      if document_version == *diagnostics_doc_version {
        return diagnostics.clone();
      }
    }
    Vec::new()
  }

  pub fn invalidate(&self, specifiers: &[ModuleSpecifier]) {
    let mut ts_diagnostics = self.0.lock();
    for specifier in specifiers {
      ts_diagnostics.remove(specifier);
    }
  }

  pub fn invalidate_all(&self) {
    self.0.lock().clear();
  }

  fn update(&self, diagnostics: &DiagnosticVec) {
    let mut stored_ts_diagnostics = self.0.lock();
    *stored_ts_diagnostics = diagnostics
      .iter()
      .map(|(specifier, version, diagnostics)| (specifier.clone(), (*version, diagnostics.clone())))
      .collect();
  }
}

pub fn should_send_diagnostic_batch_index_notifications() -> bool {
  crate::args::has_flag_env_var("DENO_DONT_USE_INTERNAL_LSP_DIAGNOSTIC_SYNC_FLAG")
}

#[derive(Clone, Debug)]
struct DiagnosticBatchCounter(Option<Arc<AtomicUsize>>);

impl Default for DiagnosticBatchCounter {
  fn default() -> Self {
    if should_send_diagnostic_batch_index_notifications() {
      Self(Some(Default::default()))
    } else {
      Self(None)
    }
  }
}

impl DiagnosticBatchCounter {
  pub fn inc(&self) -> Option<usize> {
    self.0.as_ref().map(|value| value.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1)
  }

  pub fn get(&self) -> Option<usize> {
    self.0.as_ref().map(|value| value.load(std::sync::atomic::Ordering::SeqCst))
  }
}

#[derive(Debug)]
struct ChannelMessage {
  message: DiagnosticServerUpdateMessage,
  batch_index: Option<usize>,
}

#[derive(Debug)]
pub struct DiagnosticsServer {
  channel: Option<mpsc::UnboundedSender<ChannelMessage>>,
  ts_diagnostics: TsDiagnosticsStore,
  client: Client,
  performance: Arc<Performance>,
  ts_server: Arc<TsServer>,
  batch_counter: DiagnosticBatchCounter,
}

impl DiagnosticsServer {
  pub fn new(client: Client, performance: Arc<Performance>, ts_server: Arc<TsServer>) -> Self {
    DiagnosticsServer {
      channel: Default::default(),
      ts_diagnostics: Default::default(),
      client,
      performance,
      ts_server,
      batch_counter: Default::default(),
    }
  }

  pub fn get_ts_diagnostics(&self, specifier: &ModuleSpecifier, document_version: Option<i32>) -> Vec<lsp::Diagnostic> {
    self.ts_diagnostics.get(specifier, document_version)
  }

  pub fn invalidate(&self, specifiers: &[ModuleSpecifier]) {
    self.ts_diagnostics.invalidate(specifiers);
  }

  pub fn invalidate_all(&self) {
    self.ts_diagnostics.invalidate_all();
  }

  #[allow(unused_must_use)]
  pub fn start(&mut self) {
    let (tx, mut rx) = mpsc::unbounded_channel::<ChannelMessage>();
    self.channel = Some(tx);
    let client = self.client.clone();
    let performance = self.performance.clone();
    let ts_diagnostics_store = self.ts_diagnostics.clone();
    let ts_server = self.ts_server.clone();

    let _join_handle = thread::spawn(move || {
      let runtime = create_basic_runtime();

      runtime.block_on(async {
        let mut token = CancellationToken::new();
        let mut ts_handle: Option<JoinHandle<()>> = None;
        let mut lint_handle: Option<JoinHandle<()>> = None;
        let mut deps_handle: Option<JoinHandle<()>> = None;
        let diagnostics_publisher = DiagnosticsPublisher::new(client.clone());

        loop {
          match rx.recv().await {
            // channel has closed
            None => break,
            Some(message) => {
              let ChannelMessage {
                message:
                  DiagnosticServerUpdateMessage {
                    snapshot,
                    config,
                    lint_options,
                  },
                batch_index,
              } = message;

              // cancel the previous run
              token.cancel();
              token = CancellationToken::new();
              diagnostics_publisher.clear().await;

              let previous_ts_handle = ts_handle.take();
              ts_handle = Some(spawn({
                let performance = performance.clone();
                let diagnostics_publisher = diagnostics_publisher.clone();
                let ts_server = ts_server.clone();
                let token = token.clone();
                let ts_diagnostics_store = ts_diagnostics_store.clone();
                let snapshot = snapshot.clone();
                let config = config.clone();
                async move {
                  if let Some(previous_handle) = previous_ts_handle {
                    // Wait on the previous run to complete in order to prevent
                    // multiple threads queueing up a lot of tsc requests.
                    // Do not race this with cancellation because we want a
                    // chain of events to wait for all the previous diagnostics to complete
                    previous_handle.await;
                  }

                  // Debounce timer delay. 150ms between keystrokes is about 45 WPM, so we
                  // want something that is longer than that, but not too long to
                  // introduce detectable UI delay; 200ms is a decent compromise.
                  const DELAY: Duration = Duration::from_millis(200);
                  tokio::select! {
                    _ = token.cancelled() => { return; }
                    _ = tokio::time::sleep(DELAY) => {}
                  };

                  let mark = performance.mark("update_diagnostics_ts", None::<()>);
                  let diagnostics = generate_ts_diagnostics(snapshot.clone(), &config, &ts_server, token.clone())
                    .await
                    .map_err(|err| {
                      error!("Error generating TypeScript diagnostics: {}", err);
                    })
                    .unwrap_or_default();

                  let messages_len = diagnostics.len();
                  if !token.is_cancelled() {
                    ts_diagnostics_store.update(&diagnostics);
                    diagnostics_publisher.publish(diagnostics, &token).await;

                    if !token.is_cancelled() {
                      performance.measure(mark);
                    }
                  }

                  if let Some(batch_index) = batch_index {
                    diagnostics_publisher
                      .client
                      .send_diagnostic_batch_notification(DiagnosticBatchNotificationParams { batch_index, messages_len });
                  }
                }
              }));

              let previous_deps_handle = deps_handle.take();
              deps_handle = Some(spawn({
                let performance = performance.clone();
                let diagnostics_publisher = diagnostics_publisher.clone();
                let token = token.clone();
                let snapshot = snapshot.clone();
                let config = config.clone();
                async move {
                  if let Some(previous_handle) = previous_deps_handle {
                    previous_handle.await;
                  }
                  let mark = performance.mark("update_diagnostics_deps", None::<()>);
                  let diagnostics = generate_deno_diagnostics(&snapshot, &config, token.clone()).await;

                  let messages_len = diagnostics.len();
                  if !token.is_cancelled() {
                    diagnostics_publisher.publish(diagnostics, &token).await;

                    if !token.is_cancelled() {
                      performance.measure(mark);
                    }
                  }

                  if let Some(batch_index) = batch_index {
                    diagnostics_publisher
                      .client
                      .send_diagnostic_batch_notification(DiagnosticBatchNotificationParams { batch_index, messages_len });
                  }
                }
              }));

              let previous_lint_handle = lint_handle.take();
              lint_handle = Some(spawn({
                let performance = performance.clone();
                let diagnostics_publisher = diagnostics_publisher.clone();
                let token = token.clone();
                let snapshot = snapshot.clone();
                let config = config.clone();
                async move {
                  if let Some(previous_handle) = previous_lint_handle {
                    previous_handle.await;
                  }
                  let mark = performance.mark("update_diagnostics_lint", None::<()>);
                  let diagnostics = generate_lint_diagnostics(&snapshot, &config, &lint_options, token.clone()).await;

                  let messages_len = diagnostics.len();
                  if !token.is_cancelled() {
                    diagnostics_publisher.publish(diagnostics, &token).await;

                    if !token.is_cancelled() {
                      performance.measure(mark);
                    }
                  }

                  if let Some(batch_index) = batch_index {
                    diagnostics_publisher
                      .client
                      .send_diagnostic_batch_notification(DiagnosticBatchNotificationParams { batch_index, messages_len });
                  }
                }
              }));
            }
          }
        }
      })
    });
  }

  pub fn latest_batch_index(&self) -> Option<usize> {
    self.batch_counter.get()
  }

  pub fn update(&self, message: DiagnosticServerUpdateMessage) -> Result<(), AnyError> {
    // todo(dsherret): instead of queuing up messages, it would be better to
    // instead only store the latest message (ex. maybe using a
    // tokio::sync::watch::channel)
    if let Some(tx) = &self.channel {
      tx.send(ChannelMessage {
        message,
        batch_index: self.batch_counter.inc(),
      })
      .map_err(|err| err.into())
    } else {
      Err(anyhow!("diagnostics service not started"))
    }
  }
}

impl<'a> From<&'a crate::tsc::DiagnosticCategory> for lsp::DiagnosticSeverity {
  fn from(category: &'a crate::tsc::DiagnosticCategory) -> Self {
    match category {
      crate::tsc::DiagnosticCategory::Error => lsp::DiagnosticSeverity::ERROR,
      crate::tsc::DiagnosticCategory::Warning => lsp::DiagnosticSeverity::WARNING,
      crate::tsc::DiagnosticCategory::Suggestion => lsp::DiagnosticSeverity::HINT,
      crate::tsc::DiagnosticCategory::Message => lsp::DiagnosticSeverity::INFORMATION,
    }
  }
}

impl<'a> From<&'a crate::tsc::Position> for lsp::Position {
  fn from(pos: &'a crate::tsc::Position) -> Self {
    Self {
      line: pos.line as u32,
      character: pos.character as u32,
    }
  }
}

fn get_diagnostic_message(diagnostic: &crate::tsc::Diagnostic) -> String {
  if let Some(message) = diagnostic.message_text.clone() {
    message
  } else if let Some(message_chain) = diagnostic.message_chain.clone() {
    message_chain.format_message(0)
  } else {
    "[missing message]".to_string()
  }
}

fn to_lsp_range(start: &crate::tsc::Position, end: &crate::tsc::Position) -> lsp::Range {
  lsp::Range {
    start: start.into(),
    end: end.into(),
  }
}

fn to_lsp_related_information(related_information: &Option<Vec<crate::tsc::Diagnostic>>) -> Option<Vec<lsp::DiagnosticRelatedInformation>> {
  related_information.as_ref().map(|related| {
    related
      .iter()
      .filter_map(|ri| {
        if let (Some(source), Some(start), Some(end)) = (&ri.source, &ri.start, &ri.end) {
          let uri = lsp::Url::parse(source).unwrap();
          Some(lsp::DiagnosticRelatedInformation {
            location: lsp::Location {
              uri,
              range: to_lsp_range(start, end),
            },
            message: get_diagnostic_message(ri),
          })
        } else {
          None
        }
      })
      .collect()
  })
}

fn ts_json_to_diagnostics(diagnostics: Vec<crate::tsc::Diagnostic>) -> Vec<lsp::Diagnostic> {
  diagnostics
    .iter()
    .filter_map(|d| {
      if let (Some(start), Some(end)) = (&d.start, &d.end) {
        Some(lsp::Diagnostic {
          range: to_lsp_range(start, end),
          severity: Some((&d.category).into()),
          code: Some(lsp::NumberOrString::Number(d.code as i32)),
          code_description: None,
          source: Some("deno-ts".to_string()),
          message: get_diagnostic_message(d),
          related_information: to_lsp_related_information(&d.related_information),
          tags: match d.code {
            // These are codes that indicate the variable is unused.
            2695 | 6133 | 6138 | 6192 | 6196 | 6198 | 6199 | 6205 | 7027 | 7028 => Some(vec![lsp::DiagnosticTag::UNNECESSARY]),
            // These are codes that indicated the variable is deprecated.
            2789 | 6385 | 6387 => Some(vec![lsp::DiagnosticTag::DEPRECATED]),
            _ => None,
          },
          data: None,
        })
      } else {
        None
      }
    })
    .collect()
}

async fn generate_lint_diagnostics(
  snapshot: &language_server::StateSnapshot,
  config: &ConfigSnapshot,
  lint_options: &LintOptions,
  token: CancellationToken,
) -> DiagnosticVec {
  let documents = snapshot.documents.documents(DocumentsFilter::OpenDiagnosable);
  let workspace_settings = config.settings.workspace.clone();
  let lint_rules = get_configured_rules(lint_options.rules.clone());
  let mut diagnostics_vec = Vec::new();
  if workspace_settings.lint {
    for document in documents {
      // exit early if cancelled
      if token.is_cancelled() {
        break;
      }

      // ignore any npm package files
      if let Some(node_resolver) = &snapshot.maybe_node_resolver {
        if node_resolver.in_npm_package(document.specifier()) {
          continue;
        }
      }

      let version = document.maybe_lsp_version();
      diagnostics_vec.push((
        document.specifier().clone(),
        version,
        generate_document_lint_diagnostics(config, lint_options, lint_rules.clone(), &document),
      ));
    }
  }
  diagnostics_vec
}

fn generate_document_lint_diagnostics(
  config: &ConfigSnapshot,
  lint_options: &LintOptions,
  lint_rules: Vec<&'static dyn LintRule>,
  document: &Document,
) -> Vec<lsp::Diagnostic> {
  if !config.specifier_enabled(document.specifier()) {
    return Vec::new();
  }
  if !lint_options.files.matches_specifier(document.specifier()) {
    return Vec::new();
  }
  match document.maybe_parsed_source() {
    Some(Ok(parsed_source)) => {
      if let Ok(references) = analysis::get_lint_references(&parsed_source, lint_rules) {
        references.into_iter().map(|r| r.to_diagnostic()).collect::<Vec<_>>()
      } else {
        Vec::new()
      }
    }
    Some(Err(_)) => Vec::new(),
    None => {
      error!("Missing file contents for: {}", document.specifier());
      Vec::new()
    }
  }
}

async fn generate_ts_diagnostics(
  snapshot: Arc<language_server::StateSnapshot>,
  config: &ConfigSnapshot,
  ts_server: &tsc::TsServer,
  token: CancellationToken,
) -> Result<DiagnosticVec, AnyError> {
  let mut diagnostics_vec = Vec::new();
  let specifiers = snapshot
    .documents
    .documents(DocumentsFilter::OpenDiagnosable)
    .into_iter()
    .map(|d| d.specifier().clone());
  let (enabled_specifiers, disabled_specifiers) = specifiers.into_iter().partition::<Vec<_>, _>(|s| config.specifier_enabled(s));
  let ts_diagnostics_map = if !enabled_specifiers.is_empty() {
    ts_server.get_diagnostics(snapshot.clone(), enabled_specifiers, token).await?
  } else {
    Default::default()
  };
  for (specifier_str, ts_json_diagnostics) in ts_diagnostics_map {
    let specifier = resolve_url(&specifier_str)?;
    let version = snapshot.documents.get(&specifier).and_then(|d| d.maybe_lsp_version());
    // check if the specifier is enabled again just in case TS returns us
    // diagnostics for a disabled specifier
    let ts_diagnostics = if config.specifier_enabled(&specifier) {
      ts_json_to_diagnostics(ts_json_diagnostics)
    } else {
      Vec::new()
    };
    diagnostics_vec.push((specifier, version, ts_diagnostics));
  }
  // add an empty diagnostic publish for disabled specifiers in order
  // to clear those diagnostics if they exist
  for specifier in disabled_specifiers {
    let version = snapshot.documents.get(&specifier).and_then(|d| d.maybe_lsp_version());
    diagnostics_vec.push((specifier, version, Vec::new()));
  }
  Ok(diagnostics_vec)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiagnosticDataSpecifier {
  pub specifier: ModuleSpecifier,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiagnosticDataStrSpecifier {
  pub specifier: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiagnosticDataRedirect {
  pub redirect: ModuleSpecifier,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiagnosticDataImportMapRemap {
  pub from: String,
  pub to: String,
}

/// An enum which represents diagnostic errors which originate from Deno itself.
pub enum DenoDiagnostic {
  /// A `x-deno-warning` is associated with the specifier and should be displayed
  /// as a warning to the user.
  DenoWarn(String),
  /// An informational diagnostic that indicates an existing specifier can be
  /// remapped to an import map import specifier.
  ImportMapRemap { from: String, to: String },
  /// The import assertion type is incorrect.
  InvalidAssertType(String),
  /// A module requires an assertion type to be a valid import.
  NoAssertType,
  /// A remote module was not found in the cache.
  NoCache(ModuleSpecifier),
  /// A blob module was not found in the cache.
  NoCacheBlob,
  /// A data module was not found in the cache.
  NoCacheData(ModuleSpecifier),
  /// A remote npm package reference was not found in the cache.
  NoCacheNpm(NpmPackageReqReference, ModuleSpecifier),
  /// A local module was not found on the local file system.
  NoLocal(ModuleSpecifier),
  /// The specifier resolved to a remote specifier that was redirected to
  /// another specifier.
  Redirect { from: ModuleSpecifier, to: ModuleSpecifier },
  /// An error occurred when resolving the specifier string.
  ResolutionError(deno_graph::ResolutionError),
  /// Invalid `node:` specifier.
  InvalidNodeSpecifier(ModuleSpecifier),
}

impl DenoDiagnostic {
  fn code(&self) -> &str {
    match self {
      Self::DenoWarn(_) => "deno-warn",
      Self::ImportMapRemap { .. } => "import-map-remap",
      Self::InvalidAssertType(_) => "invalid-assert-type",
      Self::NoAssertType => "no-assert-type",
      Self::NoCache(_) => "no-cache",
      Self::NoCacheBlob => "no-cache-blob",
      Self::NoCacheData(_) => "no-cache-data",
      Self::NoCacheNpm(_, _) => "no-cache-npm",
      Self::NoLocal(_) => "no-local",
      Self::Redirect { .. } => "redirect",
      Self::ResolutionError(err) => {
        if graph_util::get_resolution_error_bare_node_specifier(err).is_some() {
          "import-node-prefix-missing"
        } else {
          match err {
            ResolutionError::InvalidDowngrade { .. } => "invalid-downgrade",
            ResolutionError::InvalidLocalImport { .. } => "invalid-local-import",
            ResolutionError::InvalidSpecifier { error, .. } => match error {
              SpecifierError::ImportPrefixMissing(_, _) => "import-prefix-missing",
              SpecifierError::InvalidUrl(_) => "invalid-url",
            },
            ResolutionError::ResolverError { .. } => "resolver-error",
          }
        }
      }
      Self::InvalidNodeSpecifier(_) => "resolver-error",
    }
  }

  /// A "static" method which for a diagnostic that originated from the
  /// structure returns a code action which can resolve the diagnostic.
  pub fn get_code_action(specifier: &ModuleSpecifier, diagnostic: &lsp::Diagnostic) -> Result<lsp::CodeAction, AnyError> {
    if let Some(lsp::NumberOrString::String(code)) = &diagnostic.code {
      let code_action = match code.as_str() {
        "import-map-remap" => {
          let data = diagnostic.data.clone().ok_or_else(|| anyhow!("Diagnostic is missing data"))?;
          let DiagnosticDataImportMapRemap { from, to } = serde_json::from_value(data)?;
          lsp::CodeAction {
            title: format!("Update \"{from}\" to \"{to}\" to use import map."),
            kind: Some(lsp::CodeActionKind::QUICKFIX),
            diagnostics: Some(vec![diagnostic.clone()]),
            edit: Some(lsp::WorkspaceEdit {
              changes: Some(HashMap::from([(
                specifier.clone(),
                vec![lsp::TextEdit {
                  new_text: format!("\"{to}\""),
                  range: diagnostic.range,
                }],
              )])),
              ..Default::default()
            }),
            ..Default::default()
          }
        }
        "no-assert-type" => lsp::CodeAction {
          title: "Insert import assertion.".to_string(),
          kind: Some(lsp::CodeActionKind::QUICKFIX),
          diagnostics: Some(vec![diagnostic.clone()]),
          edit: Some(lsp::WorkspaceEdit {
            changes: Some(HashMap::from([(
              specifier.clone(),
              vec![lsp::TextEdit {
                new_text: " assert { type: \"json\" }".to_string(),
                range: lsp::Range {
                  start: diagnostic.range.end,
                  end: diagnostic.range.end,
                },
              }],
            )])),
            ..Default::default()
          }),
          ..Default::default()
        },
        "no-cache" | "no-cache-data" | "no-cache-npm" => {
          let data = diagnostic.data.clone().ok_or_else(|| anyhow!("Diagnostic is missing data"))?;
          let data: DiagnosticDataSpecifier = serde_json::from_value(data)?;
          let title = match code.as_str() {
            "no-cache" | "no-cache-npm" => {
              format!("Cache \"{}\" and its dependencies.", data.specifier)
            }
            _ => "Cache the data URL and its dependencies.".to_string(),
          };
          lsp::CodeAction {
            title,
            kind: Some(lsp::CodeActionKind::QUICKFIX),
            diagnostics: Some(vec![diagnostic.clone()]),
            command: Some(lsp::Command {
              title: "".to_string(),
              command: "deno.cache".to_string(),
              arguments: Some(vec![json!([data.specifier])]),
            }),
            ..Default::default()
          }
        }
        "redirect" => {
          let data = diagnostic.data.clone().ok_or_else(|| anyhow!("Diagnostic is missing data"))?;
          let data: DiagnosticDataRedirect = serde_json::from_value(data)?;
          lsp::CodeAction {
            title: "Update specifier to its redirected specifier.".to_string(),
            kind: Some(lsp::CodeActionKind::QUICKFIX),
            diagnostics: Some(vec![diagnostic.clone()]),
            edit: Some(lsp::WorkspaceEdit {
              changes: Some(HashMap::from([(
                specifier.clone(),
                vec![lsp::TextEdit {
                  new_text: format!("\"{}\"", data.redirect),
                  range: diagnostic.range,
                }],
              )])),
              ..Default::default()
            }),
            ..Default::default()
          }
        }
        "import-node-prefix-missing" => {
          let data = diagnostic.data.clone().ok_or_else(|| anyhow!("Diagnostic is missing data"))?;
          let data: DiagnosticDataStrSpecifier = serde_json::from_value(data)?;
          lsp::CodeAction {
            title: format!("Update specifier to node:{}", data.specifier),
            kind: Some(lsp::CodeActionKind::QUICKFIX),
            diagnostics: Some(vec![diagnostic.clone()]),
            edit: Some(lsp::WorkspaceEdit {
              changes: Some(HashMap::from([(
                specifier.clone(),
                vec![lsp::TextEdit {
                  new_text: format!("\"node:{}\"", data.specifier),
                  range: diagnostic.range,
                }],
              )])),
              ..Default::default()
            }),
            ..Default::default()
          }
        }
        _ => return Err(anyhow!("Unsupported diagnostic code (\"{}\") provided.", code)),
      };
      Ok(code_action)
    } else {
      Err(anyhow!("Unsupported diagnostic code provided."))
    }
  }

  /// Given a reference to the code from an LSP diagnostic, determine if the
  /// diagnostic is fixable or not
  pub fn is_fixable(diagnostic: &lsp_types::Diagnostic) -> bool {
    if let Some(lsp::NumberOrString::String(code)) = &diagnostic.code {
      matches!(
        code.as_str(),
        "import-map-remap" | "no-cache" | "no-cache-npm" | "no-cache-data" | "no-assert-type" | "redirect" | "import-node-prefix-missing"
      )
    } else {
      false
    }
  }

  /// Convert to an lsp Diagnostic when the range the diagnostic applies to is
  /// provided.
  pub fn to_lsp_diagnostic(&self, range: &lsp::Range) -> lsp::Diagnostic {
    let (severity, message, data) = match self {
      Self::DenoWarn(message) => (lsp::DiagnosticSeverity::WARNING, message.to_string(), None),
      Self::ImportMapRemap { from, to } => (lsp::DiagnosticSeverity::HINT, format!("The import specifier can be remapped to \"{to}\" which will resolve it via the active import map."), Some(json!({ "from": from, "to": to }))),
      Self::InvalidAssertType(assert_type) => (lsp::DiagnosticSeverity::ERROR, format!("The module is a JSON module and expected an assertion type of \"json\". Instead got \"{assert_type}\"."), None),
      Self::NoAssertType => (lsp::DiagnosticSeverity::ERROR, "The module is a JSON module and not being imported with an import assertion. Consider adding `assert { type: \"json\" }` to the import statement.".to_string(), None),
      Self::NoCache(specifier) => (lsp::DiagnosticSeverity::ERROR, format!("Uncached or missing remote URL: \"{specifier}\"."), Some(json!({ "specifier": specifier }))),
      Self::NoCacheBlob => (lsp::DiagnosticSeverity::ERROR, "Uncached blob URL.".to_string(), None),
      Self::NoCacheData(specifier) => (lsp::DiagnosticSeverity::ERROR, "Uncached data URL.".to_string(), Some(json!({ "specifier": specifier }))),
      Self::NoCacheNpm(pkg_ref, specifier) => (lsp::DiagnosticSeverity::ERROR, format!("Uncached or missing npm package: \"{}\".", pkg_ref.req), Some(json!({ "specifier": specifier }))),
      Self::NoLocal(specifier) => (lsp::DiagnosticSeverity::ERROR, format!("Unable to load a local module: \"{specifier}\".\n  Please check the file path."), None),
      Self::Redirect { from, to} => (lsp::DiagnosticSeverity::INFORMATION, format!("The import of \"{from}\" was redirected to \"{to}\"."), Some(json!({ "specifier": from, "redirect": to }))),
      Self::ResolutionError(err) => (
        lsp::DiagnosticSeverity::ERROR,
        enhanced_resolution_error_message(err),
        graph_util::get_resolution_error_bare_node_specifier(err)
          .map(|specifier| json!({ "specifier": specifier }))
      ),
      Self::InvalidNodeSpecifier(specifier) => (lsp::DiagnosticSeverity::ERROR, format!("Unknown Node built-in module: {}", specifier.path()), None),
    };
    lsp::Diagnostic {
      range: *range,
      severity: Some(severity),
      code: Some(lsp::NumberOrString::String(self.code().to_string())),
      source: Some("deno".to_string()),
      message,
      data,
      ..Default::default()
    }
  }
}

fn diagnose_resolution(
  lsp_diagnostics: &mut Vec<lsp::Diagnostic>,
  snapshot: &language_server::StateSnapshot,
  resolution: &Resolution,
  is_dynamic: bool,
  maybe_assert_type: Option<&str>,
  ranges: Vec<lsp::Range>,
) {
  let mut diagnostics = vec![];
  match resolution {
    Resolution::Ok(resolved) => {
      let specifier = &resolved.specifier;
      // If the module is a remote module and has a `X-Deno-Warning` header, we
      // want a warning diagnostic with that message.
      if let Some(metadata) = snapshot.cache_metadata.get(specifier) {
        if let Some(message) = metadata.get(&cache::MetadataKey::Warning).cloned() {
          diagnostics.push(DenoDiagnostic::DenoWarn(message));
        }
      }
      if let Some(doc) = snapshot.documents.get(specifier) {
        let doc_specifier = doc.specifier();
        // If the module was redirected, we want to issue an informational
        // diagnostic that indicates this. This then allows us to issue a code
        // action to replace the specifier with the final redirected one.
        if doc_specifier != specifier {
          diagnostics.push(DenoDiagnostic::Redirect {
            from: specifier.clone(),
            to: doc_specifier.clone(),
          });
        }
        if doc.media_type() == MediaType::Json {
          match maybe_assert_type {
            // The module has the correct assertion type, no diagnostic
            Some("json") => (),
            // The dynamic import statement is missing an assertion type, which
            // we might not be able to statically detect, therefore we will
            // not provide a potentially incorrect diagnostic.
            None if is_dynamic => (),
            // The module has an incorrect assertion type, diagnostic
            Some(assert_type) => diagnostics.push(DenoDiagnostic::InvalidAssertType(assert_type.to_string())),
            // The module is missing an assertion type, diagnostic
            None => diagnostics.push(DenoDiagnostic::NoAssertType),
          }
        }
      } else if let Ok(pkg_ref) = NpmPackageReqReference::from_specifier(specifier) {
        if let Some(npm_resolver) = &snapshot.maybe_npm_resolver {
          // show diagnostics for npm package references that aren't cached
          if !npm_resolver.is_pkg_req_folder_cached(&pkg_ref.req) {
            diagnostics.push(DenoDiagnostic::NoCacheNpm(pkg_ref, specifier.clone()));
          }
        }
      } else if let Some(module_name) = specifier.as_str().strip_prefix("node:") {
        if !deno_node::is_builtin_node_module(module_name) {
          diagnostics.push(DenoDiagnostic::InvalidNodeSpecifier(specifier.clone()));
        } else if let Some(npm_resolver) = &snapshot.maybe_npm_resolver {
          // check that a @types/node package exists in the resolver
          let types_node_ref = NpmPackageReqReference::from_str("npm:@types/node").unwrap();
          if !npm_resolver.is_pkg_req_folder_cached(&types_node_ref.req) {
            diagnostics.push(DenoDiagnostic::NoCacheNpm(
              types_node_ref,
              ModuleSpecifier::parse("npm:@types/node").unwrap(),
            ));
          }
        }
      } else {
        // When the document is not available, it means that it cannot be found
        // in the cache or locally on the disk, so we want to issue a diagnostic
        // about that.
        let deno_diagnostic = match specifier.scheme() {
          "file" => DenoDiagnostic::NoLocal(specifier.clone()),
          "data" => DenoDiagnostic::NoCacheData(specifier.clone()),
          "blob" => DenoDiagnostic::NoCacheBlob,
          _ => DenoDiagnostic::NoCache(specifier.clone()),
        };
        diagnostics.push(deno_diagnostic);
      }
    }
    // The specifier resolution resulted in an error, so we want to issue a
    // diagnostic for that.
    Resolution::Err(err) => diagnostics.push(DenoDiagnostic::ResolutionError(*err.clone())),
    _ => (),
  }
  for range in ranges {
    for diagnostic in &diagnostics {
      lsp_diagnostics.push(diagnostic.to_lsp_diagnostic(&range));
    }
  }
}

/// Generate diagnostics related to a dependency. The dependency is analyzed to
/// determine if it can be remapped to the active import map as well as surface
/// any diagnostics related to the resolved code or type dependency.
fn diagnose_dependency(
  diagnostics: &mut Vec<lsp::Diagnostic>,
  snapshot: &language_server::StateSnapshot,
  referrer: &ModuleSpecifier,
  dependency_key: &str,
  dependency: &deno_graph::Dependency,
) {
  if let Some(npm_resolver) = &snapshot.maybe_npm_resolver {
    if npm_resolver.in_npm_package(referrer) {
      return; // ignore, surface typescript errors instead
    }
  }

  if let Some(import_map) = &snapshot.maybe_import_map {
    if let Resolution::Ok(resolved) = &dependency.maybe_code {
      if let Some(to) = import_map.lookup(&resolved.specifier, referrer) {
        if dependency_key != to {
          diagnostics.push(
            DenoDiagnostic::ImportMapRemap {
              from: dependency_key.to_string(),
              to,
            }
            .to_lsp_diagnostic(&documents::to_lsp_range(&resolved.range)),
          );
        }
      }
    }
  }
  diagnose_resolution(
    diagnostics,
    snapshot,
    if dependency.maybe_code.is_none() {
      &dependency.maybe_type
    } else {
      &dependency.maybe_code
    },
    dependency.is_dynamic,
    dependency.maybe_assert_type.as_deref(),
    dependency.imports.iter().map(|i| documents::to_lsp_range(&i.range)).collect(),
  );
  // TODO(nayeemrmn): This is a crude way of detecting `@deno-types` which has
  // a different specifier and therefore needs a separate call to
  // `diagnose_resolution()`. It would be much cleaner if that were modelled as
  // a separate dependency: https://github.com/denoland/deno_graph/issues/247.
  if !dependency.maybe_type.is_none()
    && !dependency
      .imports
      .iter()
      .any(|i| dependency.maybe_type.includes(&i.range.start).is_some())
  {
    let range = match &dependency.maybe_type {
      Resolution::Ok(resolved) => documents::to_lsp_range(&resolved.range),
      Resolution::Err(error) => documents::to_lsp_range(error.range()),
      Resolution::None => unreachable!(),
    };
    diagnose_resolution(
      diagnostics,
      snapshot,
      &dependency.maybe_type,
      dependency.is_dynamic,
      dependency.maybe_assert_type.as_deref(),
      vec![range],
    );
  }
}

/// Generate diagnostics that come from Deno module resolution logic (like
/// dependencies) or other Deno specific diagnostics, like the ability to use
/// an import map to shorten an URL.
async fn generate_deno_diagnostics(snapshot: &language_server::StateSnapshot, config: &ConfigSnapshot, token: CancellationToken) -> DiagnosticVec {
  let mut diagnostics_vec = Vec::new();

  for document in snapshot.documents.documents(DocumentsFilter::OpenDiagnosable) {
    if token.is_cancelled() {
      break;
    }
    let mut diagnostics = Vec::new();
    let specifier = document.specifier();
    if config.specifier_enabled(specifier) {
      for (dependency_key, dependency) in document.dependencies() {
        diagnose_dependency(&mut diagnostics, snapshot, specifier, dependency_key, dependency);
      }
    }
    diagnostics_vec.push((specifier.clone(), document.maybe_lsp_version(), diagnostics));
  }

  diagnostics_vec
}
