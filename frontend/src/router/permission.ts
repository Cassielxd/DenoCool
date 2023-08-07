import router from "@/router"
import { useUserStoreHook } from "@/store/modules/user"
import { usePermissionStoreHook } from "@/store/modules/permission"
import { ElMessage } from "element-plus"
import { whiteList } from "@/config/white-list"
import { getToken } from "@/utils/cache/cookies"
import asyncRouteSettings from "@/config/async-route"
import NProgress from "nprogress"
import "nprogress/nprogress.css"

NProgress.configure({ showSpinner: false })

router.beforeEach(async (to, _from, next) => {
  const permissionStore = usePermissionStoreHook()
  // 判断该用户是否登录
  permissionStore.setRoutes(["admin"])
  // 将'有访问权限的动态路由' 添加到 Router 中
  permissionStore.dynamicRoutes.forEach((route) => {
    router.addRoute(route)
  })
  // 确保添加路由已完成
  // 设置 replace: true, 因此导航将不会留下历史记录
  next()

})

router.afterEach(() => {
  NProgress.done()
})
