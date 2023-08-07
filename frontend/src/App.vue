<script lang="ts" setup>
import { h } from "vue"
import { useTheme } from "@/hooks/useTheme"
import { ElNotification } from "element-plus"
// 将 Element Plus 的语言设置为中文
import zhCn from "element-plus/lib/locale/lang/zh-cn"

const { initTheme } = useTheme()

/** 初始化主题 */
initTheme()

/** 作者小心思 */
ElNotification({
  title: "大家好",
  message: h(
    "a",
    { style: "color: teal", target: "_blank", href: "_" },
    "这是项目演示版本，想要获取项目相关资料、请加入群 QQ:435604279 "
  ),
  duration: 0,
  position: "bottom-right"
})
</script>

<template>
  <ElConfigProvider :locale="zhCn">
    <router-view />
  </ElConfigProvider>
</template>
