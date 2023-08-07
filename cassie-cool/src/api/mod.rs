use actix_web::web;

pub mod code_controller;
pub mod runtime_controller;

use crate::api::code_controller::{file_tree, get_code, operation, update_content};
use crate::api::runtime_controller::{get_runtime_info, start_pro_runtime, stop_pro_runtime};
use runtime_controller::{exit, start_runtime, stop_runtime};

use self::runtime_controller::start_debugger_runtime;

pub fn api_routers(cfg: &mut web::ServiceConfig) {
  cfg
    .service(
      web::scope("/runtime")
        .service(start_runtime)
        .service(stop_runtime)
        .service(start_pro_runtime)
        .service(stop_pro_runtime)
        .service(start_debugger_runtime)
        .service(exit)
        .service(get_runtime_info),
    )
    .service(
      web::scope("/code")
        .service(get_code)
        .service(update_content)
        .service(file_tree)
        .service(operation),
    );
}
