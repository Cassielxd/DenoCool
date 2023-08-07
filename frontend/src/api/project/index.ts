import { request } from "@/utils/service"

/** 获取Ts代碼 */
export function getTsCode() {
  return request({
    url: "admin/code/get",
    method: "get"
  })
}

/** 登录并返回 Token */
export function saveTsCode(data: String) {
  return request<Login.LoginResponseData>({
    url: "admin/code/save",
    method: "post",
    data: {
      code: data
    }
  })
}

export function startRuntime(product_code: String) {
  return request({
    url: "admin/runtime/" + product_code + "/start",
    method: "get"
  })
}

export function stopRuntime(product_code: String) {
  return request({
    url: "admin/runtime/" + product_code + "/stop",
    method: "get"
  })
}

export function exit(product_code: String) {
  return request({
    url: "admin/runtime/" + product_code + "/exit",
    method: "get"
  })
}
export function getRuntimeInfo(product_code: String) {
  return request({
    url: "admin/runtime/" + product_code + "/info",
    method: "get"
  })
}
