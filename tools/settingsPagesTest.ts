import { testApi } from "./settingsPagesIpc.mjs";
import { navigate } from "../src/lib/settingsNavigation";
import app from "../src/settings/main";

// 真实设置组件已由 main.ts 挂载；这里只暴露内存 IPC 控制和生产导航/保存接口。
Object.defineProperty(window, "__settingsPagesTest", {
  value: { ...testApi, navigate, save: app.save },
  configurable: true,
});
