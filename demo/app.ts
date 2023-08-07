import { Application, Router } from "https://deno.land/x/oak@v12.5.0/mod.ts";
import chalk from "npm:chalk@5";
import { Database, MySQLConnector ,Model,DataTypes  } from 'https://deno.land/x/denodb@v1.4.0/mod.ts';
const connector = new MySQLConnector({
  database: 'low_code',
  host: '127.0.0.1',
  username: 'root',
  password: 'root',
  port: 3306, // optional
});

const db = new Database(connector);

class SysLanguage extends Model {
  static table = 'sys_language';
  static fields = {
    table_name: DataTypes.STRING,
    table_id: DataTypes.STRING,
    field_name: DataTypes.STRING,
    field_value: DataTypes.STRING,
    language: DataTypes.STRING,
  };
}
db.link([SysLanguage]);
db.sync();
console.log(chalk.green("Hello!"));


import data from "./data.json" assert { type: "json" };
const books = new Map<string, any>();
books.set("1", {
  id: "1",
  title: "The Hound of the Baskervilles",
  author: "Conan Doyle, Arthur",
});

const router = new Router();
let users1: any = null;
router.get("/language", async (context) => {
 const users = await SysLanguage.all();
  context.response.body = users;
});
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
router.get("/json", (context) => {
  context.response.body = data;
});

router.get("/book/:id", (context) => {
  if (books.has(context?.params?.id)) {
    context.response.body = books.get(context.params.id);
  }
});
router.get("/exit", (ctx) => {
  ctx.response.body = Array.from(books.values());
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
app.listen({port:3000});
