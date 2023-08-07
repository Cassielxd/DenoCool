import { Application, Router } from "https://deno.land/x/oak@v12.5.0/mod.ts";
const books = new Map<string, any>();
books.set("1", {
    id: "1",
    title: "這是一個測試數據",
    author: "348040933@qq.com",
});

const router = new Router();

router.get("/", (ctx) => {
    ctx.response.body = `<!DOCTYPE html>
    <html>
      <head><title>Hello oak!</title><head>
      <body>
        <h1>Hello oak!</h1>
      </body>
    </html>
  `;
});
router.get("/book", (context) => {
    context.response.body = Array.from(books.values());
});
router.get("/gitlib", async (context) => {
    try {
        const jsonResponse = await fetch("https://api.github.com/users/denoland");
        const jsonData = await jsonResponse.json();
        context.response.body = jsonData;
    } catch (err) {
        context.response.status = 500;
        context.response.body = { msg: err.message };
    }

});

router.get("/book/:id", (context) => {
    if (books.has(context?.params?.id)) {
        context.response.body = books.get(context.params.id);
    }
});

const app = new Application();
// Logger
app.use(async (ctx, next) => {
    await next();
    const rt = ctx.response.headers.get("X-Response-Time");
    console.log(`${ctx.request.method} ${ctx.request.url} - ${rt}`);
});

// Timing
app.use(async (ctx, next) => {
    const start = Date.now();
    await next();
    const ms = Date.now() - start;
    ctx.response.headers.set("X-Response-Time", `${ms}ms`);
});
app.use(router.routes());
app.use(router.allowedMethods());

console.log("开始监听");
//这里的端口可以不填 只有在deno运行时底下是生效的 在当前的这个魔改后的版本是无效的
app.listen({ port: 3000 });
