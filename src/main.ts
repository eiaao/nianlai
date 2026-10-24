// 先引入主题变量和全局基础样式，组件样式排在它们后面。
import "./lib/theme.css";
import "./app.css";
import { mount } from "svelte";
import Panel from "./lib/panel/Panel.svelte";

const app = mount(Panel, { target: document.getElementById("app")! });

export default app;
