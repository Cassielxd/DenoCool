pub mod api;
pub mod worker_util;

use worker_util::{ScriptWorkerId, WorkerPort, PORT_TABLE};

use actix_web::{dev::PeerAddr, error, web, Error, HttpRequest, HttpResponse};
use awc::Client;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use url::Url;
///路由转发
pub async fn forward(req: HttpRequest, payload: web::Payload, peer_addr: Option<PeerAddr>, client: web::Data<Client>) -> Result<HttpResponse, Error> {
  let product_code = match req.headers().get("product_code") {
    Some(p) => p.to_str().unwrap(),
    None => {
      return Ok(HttpResponse::NotFound().body("product_code not found"));
    }
  };
  let id = ScriptWorkerId(product_code.to_string());
  let hand_port = PORT_TABLE.read().unwrap();
  let WorkerPort(port) = match hand_port.get(&id) {
    Some(p) => p,
    None => {
      return Ok(HttpResponse::NotFound().body(format!("{} service not found", product_code)));
    }
  };
  let mut new_url = Url::parse(&format!("http://127.0.0.1:{}", port)).unwrap();
  new_url.set_path(req.uri().path());
  new_url.set_query(req.uri().query());
  let forwarded_req = client.request_from(new_url.as_str(), req.head()).no_decompress();
  let forwarded_req = match peer_addr {
    Some(PeerAddr(addr)) => forwarded_req.insert_header(("x-forwarded-for", addr.ip().to_string())),
    None => forwarded_req,
  };
  let res = forwarded_req.send_stream(payload).await.map_err(error::ErrorInternalServerError)?;
  let mut client_resp = HttpResponse::build(res.status());
  for (header_name, header_value) in res.headers().iter().filter(|(h, _)| *h != "connection") {
    client_resp.insert_header((header_name.clone(), header_value.clone()));
  }
  Ok(client_resp.streaming(res))
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Res<T> {
  pub code: i32,
  pub data: T,
}

impl<T> Res<T>
where
  T: Serialize + DeserializeOwned + Clone,
{
  pub fn respond_to(self) -> HttpResponse {
    HttpResponse::Ok().content_type("application/json").body(self.to_string())
  }
}
impl<T> ToString for Res<T>
where
  T: Serialize + DeserializeOwned + Clone,
{
  fn to_string(&self) -> String {
    serde_json::to_string(self).unwrap()
  }
}
