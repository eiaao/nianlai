import { unmount } from "svelte";
import { testApi } from "./guideTestIpc.mjs";
import app from "../src/guide/main";

// main.ts 已挂载真实生产 Guide.svelte；这里只暴露内存边界控制和真实卸载。
Object.defineProperty(window, "__guideTest", {
  value: { ...testApi, app, destroy: () => unmount(app) },
  configurable: true,
});
