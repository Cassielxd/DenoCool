<script lang="ts" setup>
import { reactive, ref, onMounted } from "vue"
import { useRouter } from "vue-router"
import { startRuntime, stopRuntime, exit, getRuntimeInfo } from "@/api/project"
const loading = ref<boolean>(false)
import { ElMessage } from "element-plus"
const router = useRouter()
const form = reactive({
  name: "默认 web 项目测试",
  code: "admin",
  num: 0,
  desc: ""
})
onMounted(() => {
  getRuntimeInfo(form.code).then((res1: any) => {
    form.num = res1.data.count
    form.desc = res1.data.description
  })
})


const start = () => {
  loading.value = true
  startRuntime(form.code).then((res: any) => {
    getRuntimeInfo(form.code).then((res1: any) => {
      form.num = res1.data.count
      form.desc = res1.data.description
    })
    ElMessage.success(`${res.data}`)
    loading.value = false
  })
}
const stop = () => {
  loading.value = true
  stopRuntime(form.code).then((res: any) => {
    getRuntimeInfo(form.code).then((res1: any) => {
      form.num = res1.data.count
      form.desc = res1.data.description
    })
    ElMessage.success(`${res.data}`)
    loading.value = false
  })
}
const stopAll = () => {
  loading.value = true
  exit(form.code).then((res: any) => {
    getRuntimeInfo(form.code).then((res1: any) => {
      form.num = res1.data.count
      form.desc = res1.data.description
    })
    ElMessage.success(`${res.data}`)
    loading.value = false
  })
}
</script>

<template>
  <div class="app-container">
    <el-card style="width: 30%" v-loading="loading">
      <template #header>
        <div class="card-header">
          <span>web项目演示</span>
        </div>
      </template>
      <el-form label-width="120px">
        <el-form-item label="项目名称">
          <el-input v-model="form.name" :disabled="true" />
        </el-form-item>
        <el-form-item label="实例数量">
          <el-input-number v-model="form.num" :disabled="true" :step="1" />
        </el-form-item>
        <el-form-item label="请求限制">
          <label>{{ form.desc }}</label>
        </el-form-item>

        <el-form-item>
          <el-button type="primary" v-if="form.num === 0" @click="start">启动新的实例</el-button>
          <el-button type="warning" v-if="form.num > 0" @click="stop">停止一个实例</el-button>
          <el-button v-if="form.num > 0" @click="stopAll">停止所有服务</el-button>
        </el-form-item>
      </el-form>
    </el-card>
    <p>1：请求资源base_path=https://localhost:9999(系统默认请求路径)</p>
    <p>2：如果系统资源已经存在 会直接请求系统资源 当系统资源不存在的时候会请求对应的 项目</p>
    <p>3：如果还不存在会直接404</p>
  </div>
</template>
<style>
.card-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.text {
  font-size: 14px;
}

.item {
  margin-bottom: 18px;
}
</style>
