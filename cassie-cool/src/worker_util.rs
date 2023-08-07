use deno_core::error::AnyError;
use deno_core::error::JsError;
use deno_runtime::colors;
use deno_runtime::fmt_errors::format_js_error;
use deno_runtime::tokio_util::create_and_run_current_thread;
use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};
use service::args;
use service::args::flags_from_vec;
use service::args::DenoSubcommand;
use service::tools::run::run_script;
use service::tools::run::run_with_watch;
use service::util::v8::get_v8_flags_from_env;
use service::util::v8::init_v8_flags;
use std::sync::{Arc, Mutex, RwLock};
use std::{collections::HashMap, net::SocketAddr};
use std::{env, thread};
use tokio::net::{TcpListener, TcpStream};
use tokio::select;
pub type WorkerTable = HashMap<ScriptWorkerId, ScriptWorkerThread>;
pub type PortTable = HashMap<ScriptWorkerId, WorkerPort>;

lazy_static! {
  pub static ref WORKER_PORT: Arc<Mutex<WorkerPort>> = Arc::new(Mutex::new(WorkerPort(3000)));
  pub static ref WORKER_TABLE: Arc<Mutex<WorkerTable>> = Arc::new(Mutex::new(WorkerTable::new()));
  pub static ref PORT_TABLE: Arc<RwLock<PortTable>> = Arc::new(RwLock::new(PortTable::new()));
}

#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkerPort(pub u16);
impl WorkerPort {
  pub fn next(&self) -> Option<WorkerPort> {
    self.0.checked_add(1).map(WorkerPort)
  }
}

pub struct Terminate {
  notify_serder: async_channel::Sender<u8>, //结束当前runtime
}
///项目server 的状态
pub enum ServerStatus {
  Start, //开始接收请求
  Wait,  //暂停介绍请求
  Exit,  //销毁server
}

/// 项目runtime key
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]

pub struct ScriptWorkerId(pub String);

///项目信息
pub struct Project {
  pub name: String, //名称 一般为英文
  pub path: String, //启动项目代码路径
}
///项目woker入口
pub struct ScriptWorkerThread {
  pub id: ScriptWorkerId,                     //项目唯一标识
  pub project: Project,                       //项目基本信息
  pub port: WorkerPort,                       //项目server端口
  pub open_debug_server: bool,                //是否debugger 启动
  pub worker_handlers: Mutex<Vec<Terminate>>, //生产环境下时 多个runtme的句柄
  stream_rx: async_channel::Receiver<TcpStream>,
  server_tx: async_channel::Sender<ServerStatus>,    // server状态通道 控制服务状态
  pub watch_tx: Option<async_channel::Sender<bool>>, //热加载模式时使用
}
impl ScriptWorkerThread {
  ///创建一个新的 worker
  /// project项目信息
  pub fn new(project: Project) -> Self {
    let (server_tx, server_rx) = async_channel::bounded::<ServerStatus>(1);
    let (stream_tx, stream_rx) = async_channel::unbounded::<TcpStream>();
    let thread_name = project.name.clone();
    let port = get_next_port(&project);
    //异步启动当前worker server
    tokio::spawn(async move {
      let addr: SocketAddr = SocketAddr::from(([127, 0, 0, 1], port.0));
      let tcp_listener = TcpListener::bind(addr).await.unwrap();
      println!("starting {} HTTP server at http://127.0.0.1:{}", thread_name, port.0);
      let mut ok = false;
      loop {
        select!(
            Ok((tcp_stream,_add))= tcp_listener.accept() => {
              if ok {
                let _ = tcp_stream.try_write(b"\xE5\x81\x9C\xE6\xAD\xA2\xE6\x9C\x8D\xE5\x8A\xA1");
              }else{
                let _ = stream_tx.send(tcp_stream).await;
              }
            }
            Ok(item) = server_rx.recv() => {
               match item{
                ServerStatus::Start => {
                  ok=false;
                },
                ServerStatus::Wait => {
                  ok=true;
                },
                ServerStatus::Exit => {
                  println!("stop {} HTTP server at http://127.0.0.1:{}", thread_name, port.0);
                  break;
                },
              }
            }
        );
      }
    });
    Self {
      id: ScriptWorkerId(project.name.clone()),
      stream_rx,
      server_tx,
      port,
      project,
      open_debug_server: false,
      watch_tx: None,
      worker_handlers: Mutex::new(Vec::new()),
    }
  }
  ///停止开发服务
  pub fn stop_watch_runtime(&mut self) {
    let watch_tx_ref = self.watch_tx.clone();
    self.watch_tx = None;
    let server_tx_ref = self.server_tx.clone();
    tokio::task::spawn(async move {
      if let Some(sender) = watch_tx_ref {
        let _ = sender.send(true).await;
        let _ = server_tx_ref.send(ServerStatus::Wait).await;
      }
    });
  }

  ///启动开发服务
  ///代码修改会直接重启服务
  pub async fn start_watch_runtime(&mut self) {
    let stream_rx = self.stream_rx.clone();
    let (watch_tx, watch_rx) = async_channel::bounded::<bool>(1);
    let mut args: Vec<String> = env::args().collect();
    args.push("run".to_string());
    args.push("--unstable".to_string());
    args.push("--watch".to_string());
    args.push(self.project.path.clone());
    let build = thread::Builder::new().name(format!("product-{}-debugger", self.id.clone().0));
    let _ = build.spawn(|| {
      let fut = async move {
        let flags = match flags_from_vec(args) {
          Ok(flags) => flags,
          Err(err) => unwrap_or_exit(Err(AnyError::from(err))),
        };
        let default_v8_flags = match flags.subcommand {
          DenoSubcommand::Lsp => vec!["--max-old-space-size=3072".to_string()],
          _ => vec![],
        };
        init_v8_flags(&default_v8_flags, &flags.v8_flags, get_v8_flags_from_env());
        //Script Engine Start
        let code = run_with_watch(flags, stream_rx, watch_rx).await;
        let handle = thread::current();
        let name = handle.name().unwrap();
        println!("{}  Worker stop info {:?}", name, code);
      };
      create_and_run_current_thread(fut);
    });
    self.watch_tx = Some(watch_tx);
    let _ = self.server_tx.send(ServerStatus::Start).await;
  }
  ///启动调试模式
  pub async fn start_debugger_runtime(&mut self) {
    let size: usize = self.worker_handlers.lock().unwrap().len();
    //如果没有启动调试服务
    if size == 0 {
      self.open_debug_server = true;
      self.start_runtime().await;
    }
  }
  ///生产环境可以启动
  pub async fn start_runtime(&mut self) {
    let size = self.worker_handlers.lock().unwrap().len();
    let stream_rx = self.stream_rx.clone();
    let (notify_tx, notify_rx) = async_channel::bounded::<u8>(1);
    let mut args: Vec<String> = env::args().collect();
    args.push("run".to_string());
    args.push(self.project.path.clone());
    let open_debug_server = self.open_debug_server;
    let build = thread::Builder::new().name(format!("product-{}-{}", self.id.clone().0, size));
    let _ = build.spawn(move || {
      let fut = async move {
        let mut flags: args::Flags = match flags_from_vec(args) {
          Ok(flags) => flags,
          Err(err) => unwrap_or_exit(Err(AnyError::from(err))),
        };
        let default_v8_flags = match flags.subcommand {
          DenoSubcommand::Lsp => vec!["--max-old-space-size=3072".to_string()],
          _ => vec![],
        };
        init_v8_flags(&default_v8_flags, &flags.v8_flags, get_v8_flags_from_env());
        flags.unstable = true;
        //开启 debugger
        if open_debug_server {
          let default = || "127.0.0.1:9229".parse::<SocketAddr>().unwrap();
          flags.inspect = Some(default());
        }
        let code = run_script(flags, stream_rx, notify_rx).await;
        let handle = thread::current();
        let name = handle.name().unwrap();
        println!("{}  Worker stop info {:?}", name, code);
      };
      create_and_run_current_thread(fut);
    });
    let mut harr: std::sync::MutexGuard<'_, Vec<Terminate>> = self.worker_handlers.lock().unwrap();
    harr.push(Terminate { notify_serder: notify_tx });
    if size == 0 {
      let _ = self.server_tx.send(ServerStatus::Start).await;
    }
  }
  ///停止runtime
  pub fn stop_runtime(&mut self) -> bool {
    let mut harr = self.worker_handlers.lock().unwrap();
    if let Some(hand) = &harr.pop() {
      let len = harr.len();
      let notify_serder = hand.notify_serder.clone();
      let server_tx_ref = self.server_tx.clone();
      tokio::task::spawn(async move {
        //停止runtime
        let _ = notify_serder.send(1).await;
        let _ = notify_serder.close();
        //如果没有runtime在运行 则暂停接收请求
        if len == 0 {
          let _ = server_tx_ref.send(ServerStatus::Wait).await;
        }
      });
      return true;
    }
    false
  }
  pub fn stop_all_runtime(&mut self) {
    self.stop_watch_runtime();
    loop {
      if !self.stop_runtime() {
        break;
      }
    }
  }
}
///Clear Script Engine Exit service
impl Drop for ScriptWorkerThread {
  fn drop(&mut self) {
    //清除当前server port标识 清楚后再不接受前端请求
    let mut hand_port = PORT_TABLE.write().unwrap();
    hand_port.remove(&self.id);
    //挺尸所有runtime
    self.stop_all_runtime();
    //停止server 服务
    let _ = self.server_tx.send_blocking(ServerStatus::Exit);
  }
}

fn unwrap_or_exit<T>(result: Result<T, AnyError>) -> T {
  match result {
    Ok(value) => value,
    Err(error) => {
      let mut error_string = format!("{error:?}");
      let mut error_code = 1;
      if let Some(e) = error.downcast_ref::<JsError>() {
        error_string = format_js_error(e);
      } else if let Some(e) = error.downcast_ref::<args::LockfileError>() {
        error_string = e.to_string();
        error_code = 10;
      }
      eprintln!("{}: {}", colors::red_bold("error"), error_string.trim_start_matches("error: "));
      std::process::exit(error_code);
    }
  }
}
use port_selector::{is_free, Port};
fn get_next_port(project: &Project) -> WorkerPort {
  let mut curport = WORKER_PORT.lock().unwrap();
  let mut curr_port = curport.next().unwrap();
  //进行端口检测 如果有被占用的情况获取下一个
  while let Some(port) = curport.next() {
    let check_port: Port = port.0;
    if is_free(check_port) {
      curr_port = port;
      break;
    }
  }
  *curport = curr_port.clone();
  let mut hand_port = PORT_TABLE.write().unwrap();
  hand_port.insert(ScriptWorkerId(project.name.clone()), curr_port.clone());
  return curr_port;
}
