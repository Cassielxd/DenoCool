use crate::{worker_util, Res};
use actix_web::{get, web, HttpResponse};
use serde::{Deserialize, Serialize};
use worker_util::{Project, ScriptWorkerId, ScriptWorkerThread, WORKER_TABLE};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WorkerInfo {
  count: usize,
  code: String,
  description: String,
}

#[get("/{product_code}/info")]
pub async fn get_runtime_info(path: web::Path<(String,)>) -> HttpResponse {
  let params = path.into_inner().0;
  let mut script_table = WORKER_TABLE.lock().unwrap();
  let work = script_table.get_mut(&ScriptWorkerId(params.clone()));

  match work {
    None => {
      return Res {
        code: 0,
        data: WorkerInfo {
          count: 0,
          code: params,
          description: "暂无实例".to_string(),
        },
      }
      .respond_to();
    }
    Some(w) => {
      let mut count = w.worker_handlers.lock().unwrap().len();
      if count == 0 && w.watch_tx.is_some() {
        count = 1;
      }
      return Res {
        code: 0,
        data: WorkerInfo {
          count: count,
          code: params.clone(),
          description: format!("请求头上添加 product_code={}", params),
        },
      }
      .respond_to();
    }
  }
}

#[get("/{product_code}/restart")]
pub async fn restart_runtime(path: web::Path<(String,)>) -> HttpResponse {
  let params = path.into_inner().0;
  let mut script_table = WORKER_TABLE.lock().unwrap();
  let work = script_table.get_mut(&ScriptWorkerId(params.clone()));
  let path = format!("code/{}/app.ts", params.clone());
  match work {
    Some(w) => {
      w.stop_watch_runtime();
      w.start_watch_runtime().await;
    }
    None => {
      let mut worker: ScriptWorkerThread = ScriptWorkerThread::new(Project { name: params.clone(), path });
      worker.start_watch_runtime().await;
      script_table.insert(worker.id.clone(), worker);
    }
  }
  return Res {
    code: 0,
    data: "成功启动".to_string(),
  }
  .respond_to();
}

///启动runtime <br>
/// product_code 产品code<br>
/// script_table所有runtime集合<br>
/// cur_port当前使用的端口<br>
/// hand_port所有 runtime使用到的 port 集合
#[get("/{product_code}/start")]
pub async fn start_runtime(path: web::Path<(String,)>) -> HttpResponse {
  let params = path.into_inner().0;
  let mut script_table = WORKER_TABLE.lock().unwrap();
  let work = script_table.get_mut(&ScriptWorkerId(params.clone()));
  let path = format!("code/{}/app.ts", params.clone());
  match work {
    Some(w) => {
      if w.watch_tx.is_none() {
        w.start_watch_runtime().await;
      }
    }
    None => {
      let mut worker: ScriptWorkerThread = ScriptWorkerThread::new(Project { name: params, path });
      worker.start_watch_runtime().await;
      script_table.insert(worker.id.clone(), worker);
    }
  }
  return Res {
    code: 0,
    data: "成功启动".to_string(),
  }
  .respond_to();
}
#[get("/{product_code}/start_debugger")]
pub async fn start_debugger_runtime(path: web::Path<(String,)>) -> HttpResponse {
  let params = path.into_inner().0;
  let mut script_table = WORKER_TABLE.lock().unwrap();
  let work = script_table.get_mut(&ScriptWorkerId(params.clone()));
  let path: String = format!("code/{}/app.ts", params.clone());
  match work {
    Some(w) => {
      w.start_debugger_runtime().await;
    }
    None => {
      let mut worker: ScriptWorkerThread = ScriptWorkerThread::new(Project { name: params, path });
      worker.start_debugger_runtime().await;
      script_table.insert(worker.id.clone(), worker);
    }
  }
  return Res {
    code: 0,
    data: "成功启动".to_string(),
  }
  .respond_to();
}
///停止一个runtime <br>
/// product_code 指产品代码<br>
/// 调用一次停止一个 runtime
#[get("/{product_code}/stop")]
pub async fn stop_runtime(path: web::Path<(String,)>) -> HttpResponse {
  let mut script_table = WORKER_TABLE.lock().unwrap();
  let name = path.into_inner().0;
  let work = script_table.get_mut(&ScriptWorkerId(name));
  match work {
    Some(w) => {
      w.stop_watch_runtime();
    }
    None => {}
  }
  return Res {
    code: 0,
    data: "停止成功".to_string(),
  }
  .respond_to();
}

///停止服务 <br>
/// product_code 产品code
#[get("/{product_code}/exit")]
pub async fn exit(path: web::Path<(String,)>) -> HttpResponse {
  let mut script_table = WORKER_TABLE.lock().unwrap();
  let name = path.into_inner().0;
  let work: Option<ScriptWorkerThread> = script_table.remove(&ScriptWorkerId(name));
  match work {
    Some(w) => {
      drop(w);
      return Res {
        code: 0,
        data: "End all processes".to_string(),
      }
      .respond_to();
    }
    None => {
      return Res {
        code: 0,
        data: "The process has ended ".to_string(),
      }
      .respond_to();
    }
  }
}

#[get("/pro/{product_code}/restart")]
pub async fn restart_pro_runtime(path: web::Path<(String,)>) -> HttpResponse {
  let params = path.into_inner().0;
  let mut script_table = WORKER_TABLE.lock().unwrap();
  let work = script_table.get_mut(&ScriptWorkerId(params.clone()));
  let path = format!("code/{}/app.ts", params.clone());
  match work {
    Some(w) => {
      w.start_runtime().await;
    }
    None => {
      let mut worker: ScriptWorkerThread = ScriptWorkerThread::new(Project { name: params.clone(), path });
      worker.start_runtime().await;
      script_table.insert(worker.id.clone(), worker);
    }
  }
  return Res {
    code: 0,
    data: "成功启动".to_string(),
  }
  .respond_to();
}

///启动runtime <br>
/// product_code 产品code<br>
/// script_table所有runtime集合<br>
/// cur_port当前使用的端口<br>
/// hand_port所有 runtime使用到的 port 集合
#[get("/pro/{product_code}/start")]
pub async fn start_pro_runtime(path: web::Path<(String,)>) -> HttpResponse {
  let params = path.into_inner().0;
  let mut script_table = WORKER_TABLE.lock().unwrap();
  let work = script_table.get_mut(&ScriptWorkerId(params.clone()));
  let path = format!("code/{}/app.ts", params.clone());

  match work {
    Some(w) => {
      w.start_runtime().await;
    }
    None => {
      let mut worker: ScriptWorkerThread = ScriptWorkerThread::new(Project { name: params.clone(), path });
      worker.start_runtime().await;
      script_table.insert(worker.id.clone(), worker);
    }
  }
  return Res {
    code: 0,
    data: "成功启动".to_string(),
  }
  .respond_to();
}

///停止一个runtime <br>
/// product_code 指产品代码<br>
/// 调用一次停止一个 runtime
#[get("/pro/{product_code}/stop")]
pub async fn stop_pro_runtime(path: web::Path<(String,)>) -> HttpResponse {
  let mut script_table = WORKER_TABLE.lock().unwrap();
  let name = path.into_inner().0;
  let work = script_table.get_mut(&ScriptWorkerId(name));
  match work {
    Some(w) => {
      w.stop_runtime();
    }
    None => {}
  }
  return Res {
    code: 0,
    data: "停止成功".to_string(),
  }
  .respond_to();
}
