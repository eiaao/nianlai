// README 配图：在演示桌面上挂载真实生产面板组件，数据来自 demoIpc。
import "../../src/lib/theme.css";
import "../../src/app.css";
import "./desk.css";
import { mount } from "svelte";
import Panel from "../../src/lib/panel/Panel.svelte";
import { demoApi } from "./demoIpc.mjs";

const params = new URLSearchParams(location.search);
document.documentElement.dataset.desk = params.get("theme") === "dark" ? "dark" : "light";
document.getElementById("desk")!.innerHTML = `
  <div class="win">
    <div class="bar"><span class="dot"></span>周会笔记.md — 记事本</div>
    <div class="doc">
      <p class="h">周会笔记</p>
      <p>· 会议地点：<span class="caret"></span></p>
      <p class="dim">· 待办：整理反馈，回复用户邮件</p>
      <p class="dim">· 截图放到共享盘</p>
    </div>
  </div>`;
mount(Panel, { target: document.getElementById("app")! });
Object.defineProperty(window, "__demo", { value: demoApi, configurable: true });
