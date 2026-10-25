// 设置窗口入口（Vite 的第二个页面 settings.html）。窗口由 Rust 侧按需创建，见 src-tauri/src/settings_window.rs。
import "../lib/theme.css";
import "./settings.css";
import { mount } from "svelte";
import Settings from "./Settings.svelte";

const app = mount(Settings, { target: document.getElementById("app")! });

export default app;
