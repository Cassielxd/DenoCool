<script lang="ts" setup>
import { nextTick, ref, onBeforeUnmount, onMounted } from "vue"
import * as monaco from "monaco-editor"
import { getTsCode } from "@/api/project"
let editor: monaco.editor.IStandaloneCodeEditor
const text = ref("")
const editorInit = () => {
  nextTick(() => {
    monaco.languages.typescript.javascriptDefaults.setDiagnosticsOptions({
      noSemanticValidation: true,
      noSyntaxValidation: false
    })
    monaco.languages.typescript.javascriptDefaults.setCompilerOptions({
      target: monaco.languages.typescript.ScriptTarget.ES2016,
      allowNonTsExtensions: true
    })
    monaco.languages.typescript.javascriptDefaults.setEagerModelSync(true)
    !editor
      ? (editor = monaco.editor.create(document.getElementById("codeEditBox") as HTMLElement, {
          value: text.value, // 编辑器初始显示文字
          language: "typescript", // 语言支持自行查阅demo
          automaticLayout: true, // 自适应布局
          theme: "vs", // 官方自带三种主题vs, hc-black, or vs-dark
          foldingStrategy: "indentation",
          renderLineHighlight: "all", // 行亮
          selectOnLineNumbers: true, // 显示行号
          minimap: {
            enabled: false
          },
          readOnly: false, // 只读
          fontSize: 16, // 字体大小
          scrollBeyondLastLine: false, // 取消代码后面一大段空白
          overviewRulerBorder: false // 不要滚动条的边框
        }))
      : editor.setValue("")

    // console.log(editor)
    // 监听值的变化
    editor.onDidChangeModelContent((val: any) => {
      text.value = editor.getValue()
    })

    editor.addAction({
      id: "save",
      keybindings: [monaco.KeyMod.chord(monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyS),],
      label: "save",
      run: () => {
        //todo 可以在这执行保存逻辑
        // console.log(editor.getValue())
      }
    })
  })
}
editorInit()
onMounted(() => {
  getTsCode().then((res) => {
    editor.setValue(res.data)
  })
})
onBeforeUnmount(() => {
  editor.dispose()
})
</script>

<template>
  <div class="app-container" style="width: 100%; height: 100%">
    <div id="codeEditBox" />
  </div>
</template>
<style>
#codeEditBox {
  height: 80%;
}
</style>
