const MonacoWebpackPlugin = require("monaco-editor-webpack-plugin");
const BundleAnalyzerPlugin = require("webpack-bundle-analyzer")
  .BundleAnalyzerPlugin;
module.exports = {
  chainWebpack: (config) => {
    config.module.rules.delete("eslint");
  },
  devServer:{
    proxy: {
      "/admin": {// api 表示拦截以 /api开头的请求路径
        target: "http://127.0.0.1:9999/",//跨域的域名（不需要写路径）
        changeOrigin: true,             //是否开启跨域
        ws: true,                       //是否代理websocked
        pathRewrite: {                  //重写路径
          ["^/admin"]: ''//把 /api 变为空字符
        }
      },
    },
  },
  configureWebpack: {
    // plugins: [new BundleAnalyzerPlugin()],
    plugins: [
      // new BundleAnalyzerPlugin(),
      new MonacoWebpackPlugin({
        // available options are documented at https://github.com/Microsoft/monaco-editor-webpack-plugin#options
        languages: [
          "typescript",
          "javascript",
          "css",
          "html",
          "json",
          "python",
          "markdown",
          "sql",
          "shell",
        ],
        features: [
          "anchorSelect",
          "bracketMatching",
          "caretOperations",
          "clipboard",
          "colorPicker",
          "cursorUndo",
          "dnd",
          "documentSymbols",
          "folding",
          "fontZoom",
          "format",
          "hover",
          "indentation",
          "inlineHints",
          "inspectTokens",
          "linesOperations",
          "linkedEditing",
          "links",
          "multicursor",
          "wordHighlighter",
        ],
      }),
    ],
    optimization: {
      splitChunks: {
        chunks: "all",
      },
    },
  },
  css: {
    loaderOptions: {
      sass: {
        //   data: `
        //     @import "@/styles/setup/_mixins.scss";
        //   `
      },
    },
  }
};
