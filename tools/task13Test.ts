import "../src/lib/theme.css";
import "../src/app.css";
import "../src/settings/settings.css";
import { mount, unmount } from "svelte";
import Panel from "../src/lib/panel/Panel.svelte";
import Settings from "../src/settings/Settings.svelte";
import { testApi } from "./task13Ipc.mjs";

const view = new URLSearchParams(location.search).get("view") ?? "panel";
const target = document.getElementById("app")!;
const apps: Parameters<typeof unmount>[0][] = [];
if (view === "pair") {
  // 同一内存后端下挂载两个真实生产窗口组件，专门验证 t3 跨窗口异步代际；不另写状态模型。
  const panelTarget = document.createElement("div");
  const settingsTarget = document.createElement("div");
  panelTarget.id = "task13-panel-window";
  settingsTarget.id = "task13-settings-window";
  settingsTarget.hidden = true;
  target.append(panelTarget, settingsTarget);
  apps.push(mount(Panel, { target: panelTarget }), mount(Settings, { target: settingsTarget }));
} else {
  apps.push(mount(view === "settings" ? Settings : Panel, { target }));
}
Object.defineProperty(window, "__task13", {
  value: { ...testApi, destroy: () => { for (const app of apps) void unmount(app); } },
  configurable: true,
});
