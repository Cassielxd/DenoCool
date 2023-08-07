use crate::Res;
use actix_web::{get, post, web, HttpRequest, HttpResponse};
use build_fs_tree::{dir, file, Build, MergeableFileSystemTree};
use serde::{Deserialize, Serialize};
use std::{
  collections::HashMap,
  path::{Path, PathBuf},
  sync::Mutex,
};
use tokio::fs::{read_to_string, remove_dir_all, remove_file, rename, File};
use walkdir::WalkDir;
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CodeFile {
  id: String,
  name: String,
  r#type: String,
  parent: String,
  parent_path: String,
  created_at: u64,
  contents: Option<String>,
}
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct OpFile {
  id: String,
  bname: Option<String>,
  cname: Option<String>,
  parent_path: String,
  r#type: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UpdateContent {
  id: Option<String>,
  contents: Option<String>,
  name: String,
  r#type: String,
  parent_path: String,
}

///获取文件内容
#[get("/{id}/get")]
pub async fn get_code(req: HttpRequest, path: web::Path<(String,)>) -> HttpResponse {
  let path_str = path.0.clone();
  let mut initial_cwd = std::env::current_dir().unwrap();
  initial_cwd.push("code");
  let product_code = match req.headers().get("product_code") {
    Some(p) => p.to_str().unwrap(),
    None => {
      return Res {
        code: 0,
        data: "product_code not found".to_string(),
      }
      .respond_to();
    }
  };
  initial_cwd.push(product_code);
  let path_str = path_str.split("|");
  path_str.for_each(|item| {
    initial_cwd.push(item);
  });

  let file = File::open(initial_cwd.clone()).await;
  match file {
    Ok(_) => {
      let contents = read_to_string(initial_cwd).await.unwrap();
      let res = Res { code: 0, data: contents };
      return res.respond_to();
    }
    Err(_) => {
      let res = Res {
        code: 0,
        data: "失敗了".to_string(),
      };
      return res.respond_to();
    }
  }
}

//文件操作
#[post("/file/{op}/operation")]
pub async fn operation(
  req: HttpRequest,
  path: web::Path<(String,)>,
  info: web::Json<OpFile>,
  file_table: web::Data<Mutex<HashMap<String, String>>>,
) -> HttpResponse {
  let action = path.0.clone();
  let mut initial_cwd = std::env::current_dir().unwrap();
  initial_cwd.push("code");
  let product_code = match req.headers().get("product_code") {
    Some(p) => p.to_str().unwrap(),
    None => {
      return Res {
        code: 0,
        data: "product_code not found".to_string(),
      }
      .respond_to();
    }
  };
  initial_cwd.push(product_code);
  let id: String = info.id.clone();
  let cname: String = info.cname.clone().unwrap_or_default();
  let parent_path: String = info.parent_path.clone();
  let parent_path = parent_path.split("|");
  parent_path.for_each(|item| {
    initial_cwd.push(item);
  });
  let isfile = match info.r#type.as_str() {
    "file" => true,
    _ => false,
  };
  let mut map = file_table.lock().unwrap();
  match action.as_str() {
    "create" => {
      if isfile {
        if cname.is_empty() {
          map.insert(id, "".to_string());
        } else {
          let _ = MergeableFileSystemTree::<String, String>::from(dir! {
            cname => file!("")
          })
          .build(initial_cwd);
        }
      } else {
        if cname.is_empty() {
          map.insert(id, "".to_string());
        } else {
          let _ = MergeableFileSystemTree::<String, String>::from(dir! {
            cname => dir!{}
          })
          .build(initial_cwd);
        }
      }
      return Res {
        code: 0,
        data: "更新成功".to_string(),
      }
      .respond_to();
    }
    "delete" => {
      if isfile {
        initial_cwd.push(cname);
        let _ = remove_file(initial_cwd).await;
      } else {
        initial_cwd.push(cname);
        let _ = remove_dir_all(initial_cwd).await;
      }
      return Res {
        code: 0,
        data: "更新成功".to_string(),
      }
      .respond_to();
    }
    "rename" => {
      match map.contains_key(&id) {
        true => {
          if isfile {
            let _ = MergeableFileSystemTree::<String, String>::from(dir! {
              cname => file!("")
            })
            .build(initial_cwd);
          } else {
            let _ = MergeableFileSystemTree::<String, String>::from(dir! {
              cname => dir!{}
            })
            .build(initial_cwd);
          }
          map.remove(&id);
        }
        false => {
          let bname: String = info.bname.clone().unwrap();
          let mut before: PathBuf = initial_cwd.clone();
          before.push(bname);
          let mut after = initial_cwd.clone();
          after.push(cname);
          let _ = rename(before.to_str().unwrap(), after.to_str().unwrap()).await;
        }
      };
    }
    _ => {}
  };
  return Res {
    code: 0,
    data: "更新成功".to_string(),
  }
  .respond_to();
}
///更新文件内容 包括新增
#[post("/update_content")]
pub async fn update_content(req: HttpRequest, info: web::Json<CodeFile>) -> HttpResponse {
  let mut initial_cwd = std::env::current_dir().unwrap();
  initial_cwd.push("code");
  let product_code = match req.headers().get("product_code") {
    Some(p) => p.to_str().unwrap(),
    None => {
      return Res {
        code: 0,
        data: "product_code not found".to_string(),
      }
      .respond_to();
    }
  };
  initial_cwd.push(product_code);
  let parent_path = info.parent_path.clone();
  let name = info.name.clone();
  let contents = info.contents.clone().unwrap_or_default();
  let parent_path = parent_path.split("|");
  parent_path.for_each(|item: &str| {
    initial_cwd.push(item);
  });
  let res = match info.r#type.as_str() {
    "file" => MergeableFileSystemTree::<String, String>::from(dir! {
      name => file!(contents)
    })
    .build(initial_cwd),
    _ => MergeableFileSystemTree::<String, String>::from(dir! {
      name => dir!{}
    })
    .build(initial_cwd),
  };
  match res {
    Ok(_) => {
      return Res {
        code: 0,
        data: "更新成功".to_string(),
      }
      .respond_to();
    }
    Err(err) => {
      return Res {
        code: -1,
        data: err.to_string(),
      }
      .respond_to();
    }
  }
}

///获取代码文件目录树
#[get("/file_tree")]
pub async fn file_tree(req: HttpRequest) -> HttpResponse {
  let mut initial_cwd = std::env::current_dir().unwrap();
  let product_code = match req.headers().get("product_code") {
    Some(p) => p.to_str().unwrap(),
    None => {
      return Res {
        code: 0,
        data: "product_code not found".to_string(),
      }
      .respond_to();
    }
  };
  let mut code_path = PathBuf::new();
  code_path.push("code");
  code_path.push(product_code.clone());
  initial_cwd.push("code");
  initial_cwd.push(product_code.clone());
  let base = initial_cwd.clone();
  let mut result = vec![];
  let mut path_map = HashMap::new();
  for entry in WalkDir::new(initial_cwd).follow_links(true).into_iter().filter_map(|e| e.ok()) {
    let metadata = entry.metadata().unwrap();
    let path = entry.path();
    if path.ends_with(product_code) {
      continue;
    }
    let (ftype, contents) = match metadata.is_dir() {
      true => ("directory".to_string(), None),
      false => {
        let contents = read_to_string(path.to_str().unwrap()).await.unwrap();
        ("file".to_string(), Some(contents))
      }
    };
    let name = entry.file_name().clone().to_str().unwrap();

    //如果是顶级目录的话为root
    let mut parent_path = "root".to_string();
    //去掉前缀
    let path = path.strip_prefix(base.clone()).unwrap();
    let ids: Vec<String> = path.iter().map(|item| item.to_str().unwrap().to_string()).collect();
    let curr_path = ids.join("|");
    let id: String = uuid::Uuid::new_v4().to_string();
    path_map.insert(curr_path.clone(), id.clone());
    if let Some(p) = path.parent() {
      if Path::new("") != p {
        let pids: Vec<String> = p.iter().map(|item| item.to_str().unwrap().to_string()).collect();
        parent_path = pids.join("|");
      }
    }
    let parent = match path_map.get(&parent_path) {
      Some(path) => path.clone(),
      None => parent_path.clone(),
    };
    result.push(CodeFile {
      id,
      name: name.to_string(),
      r#type: ftype,
      parent: parent,
      parent_path,
      created_at: 0,
      contents,
    });
  }
  return Res { code: 0, data: result }.respond_to();
}
