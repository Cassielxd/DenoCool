// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

use super::logging::lsp_log;
use crate::util::path::specifier_to_file_path;
use deno_core::error::AnyError;
use deno_core::serde::Deserialize;
use deno_core::serde::Serialize;
use deno_core::serde_json;
use deno_core::serde_json::Value;
use deno_core::ModuleSpecifier;
use lsp::Url;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Arc;
use tower_lsp::lsp_types as lsp;

pub const SETTINGS_SECTION: &str = "deno";

#[derive(Debug, Clone, Default)]
pub struct ClientCapabilities {
  pub code_action_disabled_support: bool,
  pub line_folding_only: bool,
  pub snippet_support: bool,
  pub status_notification: bool,
  /// The client provides the `experimental.testingApi` capability, which is
  /// built around VSCode's testing API. It indicates that the service should
  /// send notifications about tests discovered in modules.
  pub testing_api: bool,
  pub workspace_configuration: bool,
  pub workspace_did_change_watched_files: bool,
}

fn is_true() -> bool {
  true
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodeLensSettings {
  /// Flag for providing implementation code lenses.
  #[serde(default)]
  pub implementations: bool,
  /// Flag for providing reference code lenses.
  #[serde(default)]
  pub references: bool,
  /// Flag for providing reference code lens on all functions.  For this to have
  /// an impact, the `references` flag needs to be `true`.
  #[serde(default)]
  pub references_all_functions: bool,
  /// Flag for providing test code lens on `Deno.test` statements.  There is
  /// also the `test_args` setting, but this is not used by the service.
  #[serde(default = "is_true")]
  pub test: bool,
}

impl Default for CodeLensSettings {
  fn default() -> Self {
    Self {
      implementations: false,
      references: false,
      references_all_functions: false,
      test: true,
    }
  }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodeLensSpecifierSettings {
  /// Flag for providing test code lens on `Deno.test` statements.  There is
  /// also the `test_args` setting, but this is not used by the service.
  #[serde(default = "is_true")]
  pub test: bool,
}

impl Default for CodeLensSpecifierSettings {
  fn default() -> Self {
    Self { test: true }
  }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CompletionSettings {
  #[serde(default)]
  pub complete_function_calls: bool,
  #[serde(default = "is_true")]
  pub names: bool,
  #[serde(default = "is_true")]
  pub paths: bool,
  #[serde(default = "is_true")]
  pub auto_imports: bool,
  #[serde(default)]
  pub imports: ImportCompletionSettings,
}

impl Default for CompletionSettings {
  fn default() -> Self {
    Self {
      complete_function_calls: false,
      names: true,
      paths: true,
      auto_imports: true,
      imports: ImportCompletionSettings::default(),
    }
  }
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InlayHintsSettings {
  #[serde(default)]
  pub parameter_names: InlayHintsParamNamesOptions,
  #[serde(default)]
  pub parameter_types: InlayHintsParamTypesOptions,
  #[serde(default)]
  pub variable_types: InlayHintsVarTypesOptions,
  #[serde(default)]
  pub property_declaration_types: InlayHintsPropDeclTypesOptions,
  #[serde(default)]
  pub function_like_return_types: InlayHintsFuncLikeReturnTypesOptions,
  #[serde(default)]
  pub enum_member_values: InlayHintsEnumMemberValuesOptions,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InlayHintsParamNamesOptions {
  #[serde(default)]
  pub enabled: InlayHintsParamNamesEnabled,
  #[serde(default = "is_true")]
  pub suppress_when_argument_matches_name: bool,
}

impl Default for InlayHintsParamNamesOptions {
  fn default() -> Self {
    Self {
      enabled: InlayHintsParamNamesEnabled::None,
      suppress_when_argument_matches_name: true,
    }
  }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum InlayHintsParamNamesEnabled {
  None,
  Literals,
  All,
}

impl Default for InlayHintsParamNamesEnabled {
  fn default() -> Self {
    Self::None
  }
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InlayHintsParamTypesOptions {
  #[serde(default)]
  pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InlayHintsVarTypesOptions {
  #[serde(default)]
  pub enabled: bool,
  #[serde(default = "is_true")]
  pub suppress_when_type_matches_name: bool,
}

impl Default for InlayHintsVarTypesOptions {
  fn default() -> Self {
    Self {
      enabled: false,
      suppress_when_type_matches_name: true,
    }
  }
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InlayHintsPropDeclTypesOptions {
  #[serde(default)]
  pub enabled: bool,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InlayHintsFuncLikeReturnTypesOptions {
  #[serde(default)]
  pub enabled: bool,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InlayHintsEnumMemberValuesOptions {
  #[serde(default)]
  pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ImportCompletionSettings {
  /// A flag that indicates if non-explicitly set origins should be checked for
  /// supporting import suggestions.
  #[serde(default = "is_true")]
  pub auto_discover: bool,
  /// A map of origins which have had explicitly set if import suggestions are
  /// enabled.
  #[serde(default)]
  pub hosts: HashMap<String, bool>,
}

impl Default for ImportCompletionSettings {
  fn default() -> Self {
    Self {
      auto_discover: true,
      hosts: HashMap::default(),
    }
  }
}

/// Deno language service specific settings that can be applied uniquely to a
/// specifier.
#[derive(Debug, Default, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SpecifierSettings {
  /// A flag that indicates if Deno is enabled for this specifier or not.
  pub enable: bool,
  /// A list of paths, using the workspace folder as a base that should be Deno
  /// enabled.
  #[serde(default)]
  pub enable_paths: Vec<String>,
  /// Code lens specific settings for the resource.
  #[serde(default)]
  pub code_lens: CodeLensSpecifierSettings,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TestingSettings {
  /// A vector of arguments which should be used when running the tests for
  /// a workspace.
  #[serde(default)]
  pub args: Vec<String>,
  /// Enable or disable the testing API if the client is capable of supporting
  /// the testing API.
  #[serde(default = "is_true")]
  pub enable: bool,
}

impl Default for TestingSettings {
  fn default() -> Self {
    Self {
      args: vec!["--allow-all".to_string(), "--no-check".to_string()],
      enable: true,
    }
  }
}

fn default_to_true() -> bool {
  true
}

fn default_document_preload_limit() -> usize {
  1000
}

fn empty_string_none<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
  let o: Option<String> = Option::deserialize(d)?;
  Ok(o.filter(|s| !s.is_empty()))
}

/// Deno language service specific settings that are applied to a workspace.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSettings {
  /// A flag that indicates if Deno is enabled for the workspace.
  #[serde(default)]
  pub enable: bool,

  /// A list of paths, using the root_uri as a base that should be Deno enabled.
  #[serde(default)]
  pub enable_paths: Vec<String>,

  /// An option that points to a path string of the path to utilise as the
  /// cache/DENO_DIR for the language service.
  #[serde(default, deserialize_with = "empty_string_none")]
  pub cache: Option<String>,

  /// Override the default stores used to validate certificates. This overrides
  /// the environment variable `DENO_TLS_CA_STORE` if present.
  pub certificate_stores: Option<Vec<String>>,

  /// An option that points to a path string of the config file to apply to
  /// code within the workspace.
  #[serde(default, deserialize_with = "empty_string_none")]
  pub config: Option<String>,

  /// An option that points to a path string of the import map to apply to the
  /// code within the workspace.
  #[serde(default, deserialize_with = "empty_string_none")]
  pub import_map: Option<String>,

  /// Code lens specific settings for the workspace.
  #[serde(default)]
  pub code_lens: CodeLensSettings,

  #[serde(default)]
  pub inlay_hints: InlayHintsSettings,

  /// A flag that indicates if internal debug logging should be made available.
  #[serde(default)]
  pub internal_debug: bool,

  /// A flag that indicates if linting is enabled for the workspace.
  #[serde(default = "default_to_true")]
  pub lint: bool,

  /// Limits the number of files that can be preloaded by the language service.
  #[serde(default = "default_document_preload_limit")]
  pub document_preload_limit: usize,

  /// A flag that indicates if Dene should validate code against the unstable
  /// APIs for the workspace.
  #[serde(default)]
  pub suggest: CompletionSettings,

  /// Testing settings for the workspace.
  #[serde(default)]
  pub testing: TestingSettings,

  /// An option which sets the cert file to use when attempting to fetch remote
  /// resources. This overrides `DENO_CERT` if present.
  #[serde(default, deserialize_with = "empty_string_none")]
  pub tls_certificate: Option<String>,

  /// An option, if set, will unsafely ignore certificate errors when fetching
  /// remote resources.
  #[serde(default)]
  pub unsafely_ignore_certificate_errors: Option<Vec<String>>,

  #[serde(default)]
  pub unstable: bool,
}

impl Default for WorkspaceSettings {
  fn default() -> Self {
    WorkspaceSettings {
      enable: false,
      enable_paths: vec![],
      cache: None,
      certificate_stores: None,
      config: None,
      import_map: None,
      code_lens: Default::default(),
      inlay_hints: Default::default(),
      internal_debug: false,
      lint: true,
      document_preload_limit: default_document_preload_limit(),
      suggest: Default::default(),
      testing: Default::default(),
      tls_certificate: None,
      unsafely_ignore_certificate_errors: None,
      unstable: false,
    }
  }
}

impl WorkspaceSettings {
  /// Determine if any code lenses are enabled at all.  This allows short
  /// circuiting when there are no code lenses enabled.
  pub fn enabled_code_lens(&self) -> bool {
    self.code_lens.implementations || self.code_lens.references
  }

  /// Determine if any inlay hints are enabled. This allows short circuiting
  /// when there are no inlay hints enabled.
  pub fn enabled_inlay_hints(&self) -> bool {
    !matches!(self.inlay_hints.parameter_names.enabled, InlayHintsParamNamesEnabled::None)
      || self.inlay_hints.parameter_types.enabled
      || self.inlay_hints.variable_types.enabled
      || self.inlay_hints.property_declaration_types.enabled
      || self.inlay_hints.function_like_return_types.enabled
      || self.inlay_hints.enum_member_values.enabled
  }
}

#[derive(Debug, Clone, Default)]
pub struct ConfigSnapshot {
  pub client_capabilities: ClientCapabilities,
  pub enabled_paths: HashMap<Url, Vec<Url>>,
  pub settings: Settings,
}

impl ConfigSnapshot {
  /// Determine if the provided specifier is enabled or not.
  pub fn specifier_enabled(&self, specifier: &ModuleSpecifier) -> bool {
    if !self.enabled_paths.is_empty() {
      let specifier_str = specifier.as_str();
      for (workspace, enabled_paths) in self.enabled_paths.iter() {
        if specifier_str.starts_with(workspace.as_str()) {
          return enabled_paths.iter().any(|path| specifier_str.starts_with(path.as_str()));
        }
      }
    }
    if let Some(settings) = self.settings.specifiers.get(specifier) {
      settings.enable
    } else {
      self.settings.workspace.enable
    }
  }
}

#[derive(Debug, Default, Clone)]
pub struct Settings {
  pub specifiers: BTreeMap<ModuleSpecifier, SpecifierSettings>,
  pub workspace: WorkspaceSettings,
}

#[derive(Debug)]
pub struct Config {
  pub client_capabilities: ClientCapabilities,
  enabled_paths: HashMap<Url, Vec<Url>>,
  pub root_uri: Option<ModuleSpecifier>,
  settings: Settings,
  pub workspace_folders: Option<Vec<(ModuleSpecifier, lsp::WorkspaceFolder)>>,
}

impl Config {
  pub fn new() -> Self {
    Self {
      client_capabilities: ClientCapabilities::default(),
      enabled_paths: Default::default(),
      /// Root provided by the initialization parameters.
      root_uri: None,
      settings: Default::default(),
      workspace_folders: None,
    }
  }

  pub fn workspace_settings(&self) -> &WorkspaceSettings {
    &self.settings.workspace
  }

  /// Set the workspace settings directly, which occurs during initialization
  /// and when the client does not support workspace configuration requests
  pub fn set_workspace_settings(&mut self, value: Value) -> Result<(), AnyError> {
    let workspace_settings = serde_json::from_value(value)?;
    self.settings.workspace = workspace_settings;
    Ok(())
  }

  pub fn snapshot(&self) -> Arc<ConfigSnapshot> {
    Arc::new(ConfigSnapshot {
      client_capabilities: self.client_capabilities.clone(),
      enabled_paths: self.enabled_paths.clone(),
      settings: self.settings.clone(),
    })
  }

  pub fn has_specifier_settings(&self, specifier: &ModuleSpecifier) -> bool {
    self.settings.specifiers.contains_key(specifier)
  }

  pub fn specifier_enabled(&self, specifier: &ModuleSpecifier) -> bool {
    if !self.enabled_paths.is_empty() {
      let specifier_str = specifier.as_str();
      for (workspace, enabled_paths) in self.enabled_paths.iter() {
        if specifier_str.starts_with(workspace.as_str()) {
          return enabled_paths.iter().any(|path| specifier_str.starts_with(path.as_str()));
        }
      }
    }
    self
      .settings
      .specifiers
      .get(specifier)
      .map(|settings| settings.enable)
      .unwrap_or_else(|| self.settings.workspace.enable)
  }

  /// Gets the directories or specifically enabled file paths based on the
  /// workspace config.
  ///
  /// WARNING: This may incorrectly have some directory urls as being
  /// represented as file urls.
  pub fn enabled_urls(&self) -> Vec<Url> {
    let mut urls: Vec<Url> = Vec::new();

    if !self.settings.workspace.enable && self.enabled_paths.is_empty() {
      // do not return any urls when disabled
      return urls;
    }

    for (workspace, enabled_paths) in &self.enabled_paths {
      if !enabled_paths.is_empty() {
        urls.extend(enabled_paths.iter().cloned());
      } else {
        urls.push(workspace.clone());
      }
    }

    if urls.is_empty() {
      if let Some(root_dir) = &self.root_uri {
        urls.push(root_dir.clone())
      }
    }

    // sort for determinism
    urls.sort();
    urls
  }

  pub fn specifier_code_lens_test(&self, specifier: &ModuleSpecifier) -> bool {
    let value = self
      .settings
      .specifiers
      .get(specifier)
      .map(|settings| settings.code_lens.test)
      .unwrap_or_else(|| self.settings.workspace.code_lens.test);
    value
  }

  pub fn update_capabilities(&mut self, capabilities: &lsp::ClientCapabilities) {
    if let Some(experimental) = &capabilities.experimental {
      self.client_capabilities.status_notification = experimental.get("statusNotification").and_then(|it| it.as_bool()) == Some(true);
      self.client_capabilities.testing_api = experimental.get("testingApi").and_then(|it| it.as_bool()) == Some(true);
    }

    if let Some(workspace) = &capabilities.workspace {
      self.client_capabilities.workspace_configuration = workspace.configuration.unwrap_or(false);
      self.client_capabilities.workspace_did_change_watched_files =
        workspace.did_change_watched_files.and_then(|it| it.dynamic_registration).unwrap_or(false);
    }

    if let Some(text_document) = &capabilities.text_document {
      self.client_capabilities.line_folding_only = text_document.folding_range.as_ref().and_then(|it| it.line_folding_only).unwrap_or(false);
      self.client_capabilities.code_action_disabled_support = text_document.code_action.as_ref().and_then(|it| it.disabled_support).unwrap_or(false);
      self.client_capabilities.snippet_support = if let Some(completion) = &text_document.completion {
        completion.completion_item.as_ref().and_then(|it| it.snippet_support).unwrap_or(false)
      } else {
        false
      };
    }
  }

  /// Given the configured workspaces or root URI and the their settings,
  /// update and resolve any paths that should be enabled
  pub fn update_enabled_paths(&mut self) -> bool {
    if let Some(workspace_folders) = self.workspace_folders.clone() {
      let mut touched = false;
      for (workspace, _) in workspace_folders {
        if let Some(settings) = self.settings.specifiers.get(&workspace) {
          if self.update_enabled_paths_entry(workspace, settings.enable_paths.clone()) {
            touched = true;
          }
        }
      }
      touched
    } else if let Some(root_uri) = self.root_uri.clone() {
      self.update_enabled_paths_entry(root_uri, self.settings.workspace.enable_paths.clone())
    } else {
      false
    }
  }

  /// Update a specific entry in the enabled paths for a given workspace.
  fn update_enabled_paths_entry(&mut self, workspace: ModuleSpecifier, enabled_paths: Vec<String>) -> bool {
    let mut touched = false;
    if !enabled_paths.is_empty() {
      if let Ok(workspace_path) = specifier_to_file_path(&workspace) {
        let mut paths = Vec::new();
        for path in &enabled_paths {
          let fs_path = workspace_path.join(path);
          match ModuleSpecifier::from_file_path(fs_path) {
            Ok(path_uri) => {
              paths.push(path_uri);
            }
            Err(_) => {
              lsp_log!(
                "Unable to resolve a file path for `deno.enablePath` from \"{}\" for workspace \"{}\".",
                path,
                workspace
              );
            }
          }
        }
        if !paths.is_empty() {
          touched = true;
          self.enabled_paths.insert(workspace.clone(), paths);
        }
      }
    } else {
      touched = true;
      self.enabled_paths.remove(&workspace);
    }
    touched
  }

  pub fn get_specifiers(&self) -> Vec<ModuleSpecifier> {
    self.settings.specifiers.keys().cloned().collect()
  }

  pub fn set_specifier_settings(&mut self, specifier: ModuleSpecifier, settings: SpecifierSettings) -> bool {
    if let Some(existing) = self.settings.specifiers.get(&specifier) {
      if *existing == settings {
        return false;
      }
    }

    self.settings.specifiers.insert(specifier, settings);
    true
  }
}
