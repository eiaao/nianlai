// M2-V 门槛脚本用：通过 WebView2 的远程调试端口在样机的面板页里执行一段 JS。
// 用法：M2V_EXPR="<js>" node tools/m2v-cdp.mjs <端口>；结果打印为 JSON。需要 Node 22+ 自带的 WebSocket。
const port = process.argv[2] || "9223";
const expression = process.env.M2V_EXPR || "";
const done = (value, code = 0) => {
  console.log(JSON.stringify(value));
  process.exit(code);
};
setTimeout(() => done({ ok: false, reason: "timeout" }, 6), 15000);
const targets = await (await fetch(`http://127.0.0.1:${port}/json`)).json();
const page = targets.find((t) => t.type === "page");
if (!page) done({ ok: false, reason: "target_not_found" }, 4);
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((resolve, reject) => {
  ws.onopen = resolve;
  ws.onerror = reject;
});
let id = 0;
const send = (method, params) => ws.send(JSON.stringify({ id: ++id, method, params }));
ws.onmessage = (event) => {
  const message = JSON.parse(event.data);
  if (message.id === 1) {
    // 面板隐藏时 WebView2 被挂起，页面冻结，IPC 不会被处理；先把页面切回活跃。
    send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
    return;
  }
  if (message.id !== 2) return;
  const result = message.result;
  if (result?.exceptionDetails) done({ ok: false, exception: result.exceptionDetails.text }, 5);
  done({ ok: true, value: result?.result?.value });
};
send("Page.setWebLifecycleState", { state: "active" });
