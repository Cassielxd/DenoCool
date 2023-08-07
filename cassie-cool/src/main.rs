use std::{collections::HashMap, sync::Mutex};

use actix_governor::{GovernorConfigBuilder, Governor};
use actix_web::{middleware, web, App, HttpServer};
use awc::Client;
use cassie_cool::{api::api_routers, forward};
///网关入口0
#[tokio::main]
async fn main() -> std::io::Result<()> {
  env_logger::init_from_env(env_logger::Env::new().default_filter_or("info"));
  //在这里写 是所有线程共享
  let file_table: web::Data<Mutex<HashMap<String, String>>> = web::Data::new(Mutex::new(HashMap::new()));
  bannder();
  let  governor_conf  = GovernorConfigBuilder::default().per_second(2).burst_size(5).finish().unwrap();
  log::info!("starting main HTTP server at http://127.0.0.1:9999");
  HttpServer::new(move || {
    //在这里写  是有问题的  只会在当前线程里有效
    App::new()
      .wrap(Governor::new(&governor_conf))
      .configure(api_routers)
      .app_data(file_table.clone())
      .app_data(web::Data::new(Client::default()))
      .wrap(middleware::Logger::default())
      .default_service(web::to(forward))
  })
  .bind(("127.0.0.1", 9999))?
  .run()
  .await
}
fn bannder() {
  eprintln!(
    r#"  ______                _          _____                        ______            _ 
 / _____)              (_)        (____ \                      / _____)          | |
| /      ____  ___  ___ _  ____    _   \ \ ____ ____   ___    | /      ___   ___ | |
| |     / _  |/___)/___) |/ _  )  | |   | / _  )  _ \ / _ \   | |     / _ \ / _ \| |
| \____( ( | |___ |___ | ( (/ /   | |__/ ( (/ /| | | | |_| |  | \____| |_| | |_| | |
 \______)_||_(___/(___/|_|\____)  |_____/ \____)_| |_|\___/    \______)___/ \___/|_|
"#
  );
}
