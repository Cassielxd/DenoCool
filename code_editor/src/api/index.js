import { request } from "@/utils/service"

export function getAllFiles() {
    return request({
        url: "/admin/code/file_tree", method: "get",
        headers: {
            "product_code": "admin"
        },
    })
}

export function updateFileContent(file) {
    return request({
        url: "/admin/code/update_content", method: "post",
        headers: {
            "product_code": "admin"
        },
        data:file
    })
}

export function deleteFile(file) {
    return request({
        url: "/admin/code/file/delete/operation", method: "post",
        headers: {
            "product_code": "admin"
        },
        data:file
    })
}
export function rename(file) {
    return request({
        url: "/admin/code/file/rename/operation", method: "post",
        headers: {
            "product_code": "admin"
        },
        data:file
    })
}
export function createFile(file) {
    return request({
        url: "/admin/code/file/create/operation", method: "post",
        headers: {
            "product_code": "admin"
        },
        data:file
    })
}
