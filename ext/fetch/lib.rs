// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

mod byte_stream;
mod fs_fetch_handler;

use std::borrow::Cow;
use std::cell::RefCell;
use std::cmp::min;
use std::convert::From;
use std::path::Path;
use std::path::PathBuf;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;

use deno_core::error::type_error;
use deno_core::error::AnyError;
use deno_core::futures::stream::Peekable;
use deno_core::futures::Future;
use deno_core::futures::Stream;
use deno_core::futures::StreamExt;
use deno_core::op;
use deno_core::BufView;
use deno_core::WriteOutcome;

use deno_core::url::Url;
use deno_core::AsyncRefCell;
use deno_core::AsyncResult;
use deno_core::ByteString;
use deno_core::CancelFuture;
use deno_core::CancelHandle;
use deno_core::CancelTryFuture;
use deno_core::Canceled;
use deno_core::OpState;
use deno_core::RcRef;
use deno_core::Resource;
use deno_core::ResourceId;
use deno_core::ZeroCopyBuf;
use deno_tls::rustls::RootCertStore;
use deno_tls::Proxy;
use deno_tls::RootCertStoreProvider;

use data_url::DataUrl;
use http::header::CONTENT_LENGTH;
use http::Uri;
use reqwest::header::HeaderMap;
use reqwest::header::HeaderName;
use reqwest::header::HeaderValue;
use reqwest::header::ACCEPT_ENCODING;
use reqwest::header::HOST;
use reqwest::header::RANGE;
use reqwest::header::USER_AGENT;
use reqwest::redirect::Policy;
use reqwest::Body;
use reqwest::Client;
use reqwest::Method;
use reqwest::RequestBuilder;
use reqwest::Response;
use serde::Deserialize;
use serde::Serialize;
use tokio::sync::mpsc;

// Re-export reqwest and data_url
pub use data_url;
pub use reqwest;

pub use fs_fetch_handler::FsFetchHandler;

pub use crate::byte_stream::MpscByteStream;

#[derive(Clone)]
pub struct Options {
  pub user_agent: String,
  pub root_cert_store_provider: Option<Arc<dyn RootCertStoreProvider>>,
  pub proxy: Option<Proxy>,
  pub request_builder_hook: Option<fn(RequestBuilder) -> Result<RequestBuilder, AnyError>>,
  pub unsafely_ignore_certificate_errors: Option<Vec<String>>,
  pub client_cert_chain_and_key: Option<(String, String)>,
  pub file_fetch_handler: Rc<dyn FetchHandler>,
}

impl Options {
  pub fn root_cert_store(&self) -> Result<Option<RootCertStore>, AnyError> {
    Ok(match &self.root_cert_store_provider {
      Some(provider) => Some(provider.get_or_try_init()?.clone()),
      None => None,
    })
  }
}

impl Default for Options {
  fn default() -> Self {
    Self {
      user_agent: "".to_string(),
      root_cert_store_provider: None,
      proxy: None,
      request_builder_hook: None,
      unsafely_ignore_certificate_errors: None,
      client_cert_chain_and_key: None,
      file_fetch_handler: Rc::new(DefaultFileFetchHandler),
    }
  }
}

deno_core::extension!(deno_fetch,
  deps = [ deno_webidl, deno_web, deno_url, deno_console ],
  parameters = [FP: FetchPermissions],
  ops = [
    op_fetch<FP>,
    op_fetch_send,
    op_fetch_custom_client<FP>,
  ],
  esm = [
    "20_headers.js",
    "21_formdata.js",
    "22_body.js",
    "22_http_client.js",
    "23_request.js",
    "23_response.js",
    "26_fetch.js"
  ],
  options = {
    options: Options,
  },
  state = |state, options| {
    state.put::<Options>(options.options);
  },
);

pub type CancelableResponseFuture = Pin<Box<dyn Future<Output = CancelableResponseResult>>>;

pub trait FetchHandler: dyn_clone::DynClone {
  // Return the result of the fetch request consisting of a tuple of the
  // cancelable response result, the optional fetch body resource and the
  // optional cancel handle.
  fn fetch_file(&self, state: &mut OpState, url: Url) -> (CancelableResponseFuture, Option<FetchRequestBodyResource>, Option<Rc<CancelHandle>>);
}

dyn_clone::clone_trait_object!(FetchHandler);

/// A default implementation which will error for every request.
#[derive(Clone)]
pub struct DefaultFileFetchHandler;

impl FetchHandler for DefaultFileFetchHandler {
  fn fetch_file(&self, _state: &mut OpState, _url: Url) -> (CancelableResponseFuture, Option<FetchRequestBodyResource>, Option<Rc<CancelHandle>>) {
    let fut = async move { Ok(Err(type_error("NetworkError when attempting to fetch resource."))) };
    (Box::pin(fut), None, None)
  }
}

pub trait FetchPermissions {
  fn check_net_url(&mut self, _url: &Url, api_name: &str) -> Result<(), AnyError>;
  fn check_read(&mut self, _p: &Path, api_name: &str) -> Result<(), AnyError>;
}

pub fn get_declaration() -> PathBuf {
  PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lib.deno_fetch.d.ts")
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchReturn {
  pub request_rid: ResourceId,
  pub request_body_rid: Option<ResourceId>,
  pub cancel_handle_rid: Option<ResourceId>,
}

pub fn get_or_create_client_from_state(state: &mut OpState) -> Result<reqwest::Client, AnyError> {
  if let Some(client) = state.try_borrow::<reqwest::Client>() {
    Ok(client.clone())
  } else {
    let options = state.borrow::<Options>();
    let client = create_http_client(
      &options.user_agent,
      CreateHttpClientOptions {
        root_cert_store: options.root_cert_store()?,
        ca_certs: vec![],
        proxy: options.proxy.clone(),
        unsafely_ignore_certificate_errors: options.unsafely_ignore_certificate_errors.clone(),
        client_cert_chain_and_key: options.client_cert_chain_and_key.clone(),
        pool_max_idle_per_host: None,
        pool_idle_timeout: None,
        http1: true,
        http2: true,
      },
    )?;
    state.put::<reqwest::Client>(client.clone());
    Ok(client)
  }
}

#[op]
pub fn op_fetch<FP>(
  state: &mut OpState,
  method: ByteString,
  url: String,
  headers: Vec<(ByteString, ByteString)>,
  client_rid: Option<u32>,
  has_body: bool,
  body_length: Option<u64>,
  data: Option<ZeroCopyBuf>,
) -> Result<FetchReturn, AnyError>
where
  FP: FetchPermissions + 'static,
{
  let client = if let Some(rid) = client_rid {
    let r = state.resource_table.get::<HttpClientResource>(rid)?;
    r.client.clone()
  } else {
    get_or_create_client_from_state(state)?
  };

  let method = Method::from_bytes(&method)?;
  let url = Url::parse(&url)?;

  // Check scheme before asking for net permission
  let scheme = url.scheme();
  let (request_rid, request_body_rid, cancel_handle_rid) = match scheme {
    "file" => {
      let path = url
        .to_file_path()
        .map_err(|_| type_error("NetworkError when attempting to fetch resource."))?;
      let permissions = state.borrow_mut::<FP>();
      permissions.check_read(&path, "fetch()")?;

      if method != Method::GET {
        return Err(type_error(format!("Fetching files only supports the GET method. Received {method}.")));
      }

      let Options { file_fetch_handler, .. } = state.borrow_mut::<Options>();
      let file_fetch_handler = file_fetch_handler.clone();
      let (request, maybe_request_body, maybe_cancel_handle) = file_fetch_handler.fetch_file(state, url);
      let request_rid = state.resource_table.add(FetchRequestResource(request));
      let maybe_request_body_rid = maybe_request_body.map(|r| state.resource_table.add(r));
      let maybe_cancel_handle_rid = maybe_cancel_handle.map(|ch| state.resource_table.add(FetchCancelHandle(ch)));

      (request_rid, maybe_request_body_rid, maybe_cancel_handle_rid)
    }
    "http" | "https" => {
      let permissions = state.borrow_mut::<FP>();
      permissions.check_net_url(&url, "fetch()")?;

      // Make sure that we have a valid URI early, as reqwest's `RequestBuilder::send`
      // internally uses `expect_uri`, which panics instead of returning a usable `Result`.
      if url.as_str().parse::<Uri>().is_err() {
        return Err(type_error("Invalid URL"));
      }

      let mut request = client.request(method.clone(), url);

      let request_body_rid = if has_body {
        match data {
          None => {
            // If no body is passed, we return a writer for streaming the body.
            let (stream, tx) = MpscByteStream::new();

            // If the size of the body is known, we include a content-length
            // header explicitly.
            if let Some(body_size) = body_length {
              request = request.header(CONTENT_LENGTH, HeaderValue::from(body_size))
            }

            request = request.body(Body::wrap_stream(stream));

            let request_body_rid = state.resource_table.add(FetchRequestBodyResource {
              body: AsyncRefCell::new(tx),
              cancel: CancelHandle::default(),
            });

            Some(request_body_rid)
          }
          Some(data) => {
            // If a body is passed, we use it, and don't return a body for streaming.
            request = request.body(data.to_vec());
            None
          }
        }
      } else {
        // POST and PUT requests should always have a 0 length content-length,
        // if there is no body. https://fetch.spec.whatwg.org/#http-network-or-cache-fetch
        if matches!(method, Method::POST | Method::PUT) {
          request = request.header(CONTENT_LENGTH, HeaderValue::from(0));
        }
        None
      };

      let mut header_map = HeaderMap::new();
      for (key, value) in headers {
        let name = HeaderName::from_bytes(&key).map_err(|err| type_error(err.to_string()))?;
        let v = HeaderValue::from_bytes(&value).map_err(|err| type_error(err.to_string()))?;

        if !matches!(name, HOST | CONTENT_LENGTH) {
          header_map.append(name, v);
        }
      }

      if header_map.contains_key(RANGE) {
        // https://fetch.spec.whatwg.org/#http-network-or-cache-fetch step 18
        // If httpRequest’s header list contains `Range`, then append (`Accept-Encoding`, `identity`)
        header_map.insert(ACCEPT_ENCODING, HeaderValue::from_static("identity"));
      }
      request = request.headers(header_map);

      let options = state.borrow::<Options>();
      if let Some(request_builder_hook) = options.request_builder_hook {
        request = request_builder_hook(request).map_err(|err| type_error(err.to_string()))?;
      }

      let cancel_handle = CancelHandle::new_rc();
      let cancel_handle_ = cancel_handle.clone();

      let fut = async move {
        request
          .send()
          .or_cancel(cancel_handle_)
          .await
          .map(|res| res.map_err(|err| type_error(err.to_string())))
      };

      let request_rid = state.resource_table.add(FetchRequestResource(Box::pin(fut)));

      let cancel_handle_rid = state.resource_table.add(FetchCancelHandle(cancel_handle));

      (request_rid, request_body_rid, Some(cancel_handle_rid))
    }
    "data" => {
      let data_url = DataUrl::process(url.as_str()).map_err(|e| type_error(format!("{e:?}")))?;

      let (body, _) = data_url.decode_to_vec().map_err(|e| type_error(format!("{e:?}")))?;

      let response = http::Response::builder()
        .status(http::StatusCode::OK)
        .header(http::header::CONTENT_TYPE, data_url.mime_type().to_string())
        .body(reqwest::Body::from(body))?;

      let fut = async move { Ok(Ok(Response::from(response))) };

      let request_rid = state.resource_table.add(FetchRequestResource(Box::pin(fut)));

      (request_rid, None, None)
    }
    "blob" => {
      // Blob URL resolution happens in the JS side of fetch. If we got here is
      // because the URL isn't an object URL.
      return Err(type_error("Blob for the given URL not found."));
    }
    _ => return Err(type_error(format!("scheme '{scheme}' not supported"))),
  };

  Ok(FetchReturn {
    request_rid,
    request_body_rid,
    cancel_handle_rid,
  })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchResponse {
  pub status: u16,
  pub status_text: String,
  pub headers: Vec<(ByteString, ByteString)>,
  pub url: String,
  pub response_rid: ResourceId,
  pub content_length: Option<u64>,
}

#[op]
pub async fn op_fetch_send(state: Rc<RefCell<OpState>>, rid: ResourceId) -> Result<FetchResponse, AnyError> {
  let request = state.borrow_mut().resource_table.take::<FetchRequestResource>(rid)?;

  let request = Rc::try_unwrap(request).ok().expect("multiple op_fetch_send ongoing");

  let res = match request.0.await {
    Ok(Ok(res)) => res,
    Ok(Err(err)) => return Err(type_error(err.to_string())),
    Err(_) => return Err(type_error("request was cancelled")),
  };

  //debug!("Fetch response {}", url);
  let status = res.status();
  let url = res.url().to_string();
  let mut res_headers = Vec::new();
  for (key, val) in res.headers().iter() {
    res_headers.push((key.as_str().into(), val.as_bytes().into()));
  }

  let content_length = res.content_length();

  let stream: BytesStream = Box::pin(
    res
      .bytes_stream()
      .map(|r| r.map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err))),
  );
  let rid = state.borrow_mut().resource_table.add(FetchResponseBodyResource {
    reader: AsyncRefCell::new(stream.peekable()),
    cancel: CancelHandle::default(),
    size: content_length,
  });

  Ok(FetchResponse {
    status: status.as_u16(),
    status_text: status.canonical_reason().unwrap_or("").to_string(),
    headers: res_headers,
    url,
    response_rid: rid,
    content_length,
  })
}

type CancelableResponseResult = Result<Result<Response, AnyError>, Canceled>;

pub struct FetchRequestResource(pub Pin<Box<dyn Future<Output = CancelableResponseResult>>>);

impl Resource for FetchRequestResource {
  fn name(&self) -> Cow<str> {
    "fetchRequest".into()
  }
}

pub struct FetchCancelHandle(pub Rc<CancelHandle>);

impl Resource for FetchCancelHandle {
  fn name(&self) -> Cow<str> {
    "fetchCancelHandle".into()
  }

  fn close(self: Rc<Self>) {
    self.0.cancel()
  }
}

pub struct FetchRequestBodyResource {
  pub body: AsyncRefCell<mpsc::Sender<Option<bytes::Bytes>>>,
  pub cancel: CancelHandle,
}

impl Resource for FetchRequestBodyResource {
  fn name(&self) -> Cow<str> {
    "fetchRequestBody".into()
  }

  fn write(self: Rc<Self>, buf: BufView) -> AsyncResult<WriteOutcome> {
    Box::pin(async move {
      let bytes: bytes::Bytes = buf.into();
      let nwritten = bytes.len();
      let body = RcRef::map(&self, |r| &r.body).borrow_mut().await;
      let cancel = RcRef::map(self, |r| &r.cancel);
      body
        .send(Some(bytes))
        .or_cancel(cancel)
        .await?
        .map_err(|_| type_error("request body receiver not connected (request closed)"))?;
      Ok(WriteOutcome::Full { nwritten })
    })
  }

  fn shutdown(self: Rc<Self>) -> AsyncResult<()> {
    Box::pin(async move {
      let body = RcRef::map(&self, |r| &r.body).borrow_mut().await;
      let cancel = RcRef::map(self, |r| &r.cancel);
      // There is a case where hyper knows the size of the response body up
      // front (through content-length header on the resp), where it will drop
      // the body once that content length has been reached, regardless of if
      // the stream is complete or not. This is expected behaviour, but it means
      // that if you stream a body with an up front known size (eg a Blob),
      // explicit shutdown can never succeed because the body (and by extension
      // the receiver) will have dropped by the time we try to shutdown. As such
      // we ignore if the receiver is closed, because we know that the request
      // is complete in good health in that case.
      body.send(None).or_cancel(cancel).await?.ok();
      Ok(())
    })
  }

  fn close(self: Rc<Self>) {
    self.cancel.cancel()
  }
}

type BytesStream = Pin<Box<dyn Stream<Item = Result<bytes::Bytes, std::io::Error>> + Unpin>>;

pub struct FetchResponseBodyResource {
  pub reader: AsyncRefCell<Peekable<BytesStream>>,
  pub cancel: CancelHandle,
  pub size: Option<u64>,
}

impl Resource for FetchResponseBodyResource {
  fn name(&self) -> Cow<str> {
    "fetchResponseBody".into()
  }

  fn read(self: Rc<Self>, limit: usize) -> AsyncResult<BufView> {
    Box::pin(async move {
      let reader = RcRef::map(&self, |r| &r.reader).borrow_mut().await;

      let fut = async move {
        let mut reader = Pin::new(reader);
        loop {
          match reader.as_mut().peek_mut().await {
            Some(Ok(chunk)) if !chunk.is_empty() => {
              let len = min(limit, chunk.len());
              let chunk = chunk.split_to(len);
              break Ok(chunk.into());
            }
            // This unwrap is safe because `peek_mut()` returned `Some`, and thus
            // currently has a peeked value that can be synchronously returned
            // from `next()`.
            //
            // The future returned from `next()` is always ready, so we can
            // safely call `await` on it without creating a race condition.
            Some(_) => match reader.as_mut().next().await.unwrap() {
              Ok(chunk) => assert!(chunk.is_empty()),
              Err(err) => break Err(type_error(err.to_string())),
            },
            None => break Ok(BufView::empty()),
          }
        }
      };

      let cancel_handle = RcRef::map(self, |r| &r.cancel);
      fut.try_or_cancel(cancel_handle).await
    })
  }

  fn size_hint(&self) -> (u64, Option<u64>) {
    (self.size.unwrap_or(0), self.size)
  }

  fn close(self: Rc<Self>) {
    self.cancel.cancel()
  }
}

pub struct HttpClientResource {
  pub client: Client,
}

impl Resource for HttpClientResource {
  fn name(&self) -> Cow<str> {
    "httpClient".into()
  }
}

impl HttpClientResource {
  fn new(client: Client) -> Self {
    Self { client }
  }
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub enum PoolIdleTimeout {
  State(bool),
  Specify(u64),
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CreateHttpClientArgs {
  ca_certs: Vec<String>,
  proxy: Option<Proxy>,
  cert_chain: Option<String>,
  private_key: Option<String>,
  pool_max_idle_per_host: Option<usize>,
  pool_idle_timeout: Option<PoolIdleTimeout>,
  #[serde(default = "default_true")]
  http1: bool,
  #[serde(default = "default_true")]
  http2: bool,
}

fn default_true() -> bool {
  true
}

#[op]
pub fn op_fetch_custom_client<FP>(state: &mut OpState, args: CreateHttpClientArgs) -> Result<ResourceId, AnyError>
where
  FP: FetchPermissions + 'static,
{
  if let Some(proxy) = args.proxy.clone() {
    let permissions = state.borrow_mut::<FP>();
    let url = Url::parse(&proxy.url)?;
    permissions.check_net_url(&url, "Deno.createHttpClient()")?;
  }

  let client_cert_chain_and_key = {
    if args.cert_chain.is_some() || args.private_key.is_some() {
      let cert_chain = args.cert_chain.ok_or_else(|| type_error("No certificate chain provided"))?;
      let private_key = args.private_key.ok_or_else(|| type_error("No private key provided"))?;

      Some((cert_chain, private_key))
    } else {
      None
    }
  };

  let options = state.borrow::<Options>();
  let ca_certs = args.ca_certs.into_iter().map(|cert| cert.into_bytes()).collect::<Vec<_>>();

  let client = create_http_client(
    &options.user_agent,
    CreateHttpClientOptions {
      root_cert_store: options.root_cert_store()?,
      ca_certs,
      proxy: args.proxy,
      unsafely_ignore_certificate_errors: options.unsafely_ignore_certificate_errors.clone(),
      client_cert_chain_and_key,
      pool_max_idle_per_host: args.pool_max_idle_per_host,
      pool_idle_timeout: args.pool_idle_timeout.and_then(|timeout| match timeout {
        PoolIdleTimeout::State(true) => None,
        PoolIdleTimeout::State(false) => Some(None),
        PoolIdleTimeout::Specify(specify) => Some(Some(specify)),
      }),
      http1: args.http1,
      http2: args.http2,
    },
  )?;

  let rid = state.resource_table.add(HttpClientResource::new(client));
  Ok(rid)
}

#[derive(Debug, Clone)]
pub struct CreateHttpClientOptions {
  pub root_cert_store: Option<RootCertStore>,
  pub ca_certs: Vec<Vec<u8>>,
  pub proxy: Option<Proxy>,
  pub unsafely_ignore_certificate_errors: Option<Vec<String>>,
  pub client_cert_chain_and_key: Option<(String, String)>,
  pub pool_max_idle_per_host: Option<usize>,
  pub pool_idle_timeout: Option<Option<u64>>,
  pub http1: bool,
  pub http2: bool,
}

impl Default for CreateHttpClientOptions {
  fn default() -> Self {
    CreateHttpClientOptions {
      root_cert_store: None,
      ca_certs: vec![],
      proxy: None,
      unsafely_ignore_certificate_errors: None,
      client_cert_chain_and_key: None,
      pool_max_idle_per_host: None,
      pool_idle_timeout: None,
      http1: true,
      http2: true,
    }
  }
}

/// Create new instance of async reqwest::Client. This client supports
/// proxies and doesn't follow redirects.
pub fn create_http_client(user_agent: &str, options: CreateHttpClientOptions) -> Result<Client, AnyError> {
  let mut tls_config = deno_tls::create_client_config(
    options.root_cert_store,
    options.ca_certs,
    options.unsafely_ignore_certificate_errors,
    options.client_cert_chain_and_key,
  )?;

  let mut alpn_protocols = vec![];
  if options.http2 {
    alpn_protocols.push("h2".into());
  }
  if options.http1 {
    alpn_protocols.push("http/1.1".into());
  }
  tls_config.alpn_protocols = alpn_protocols;

  let mut headers = HeaderMap::new();
  headers.insert(USER_AGENT, user_agent.parse().unwrap());
  let mut builder = Client::builder()
    .redirect(Policy::none())
    .default_headers(headers)
    .use_preconfigured_tls(tls_config);

  if let Some(proxy) = options.proxy {
    let mut reqwest_proxy = reqwest::Proxy::all(&proxy.url)?;
    if let Some(basic_auth) = &proxy.basic_auth {
      reqwest_proxy = reqwest_proxy.basic_auth(&basic_auth.username, &basic_auth.password);
    }
    builder = builder.proxy(reqwest_proxy);
  }

  if let Some(pool_max_idle_per_host) = options.pool_max_idle_per_host {
    builder = builder.pool_max_idle_per_host(pool_max_idle_per_host);
  }

  if let Some(pool_idle_timeout) = options.pool_idle_timeout {
    builder = builder.pool_idle_timeout(pool_idle_timeout.map(std::time::Duration::from_millis));
  }

  match (options.http1, options.http2) {
    (true, false) => builder = builder.http1_only(),
    (false, true) => builder = builder.http2_prior_knowledge(),
    (true, true) => {}
    (false, false) => return Err(type_error("Either `http1` or `http2` needs to be true")),
  }

  builder.build().map_err(|e| e.into())
}
