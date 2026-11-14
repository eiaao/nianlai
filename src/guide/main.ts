// 首次启动独立窗口入口。真实状态与副作用均在 Guide.svelte / GuideFlow 中。
import "../lib/theme.css";
import { mount } from "svelte";
import Guide from "./Guide.svelte";

const app = mount(Guide, { target: document.getElementById("app")! });

export default app;
