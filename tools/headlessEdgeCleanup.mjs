import { existsSync, rmSync } from "node:fs";
import { dirname } from "node:path";
import { spawn } from "node:child_process";

const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function waitExit(child, timeout = 10000) {
  if (child.exitCode !== null || child.signalCode !== null) return true;
  const exited = new Promise((resolve) => child.once("exit", resolve));
  await Promise.race([exited, pause(timeout)]);
  return child.exitCode !== null || child.signalCode !== null;
}

async function stopTree(child) {
  if (!child?.pid) return;
  if (process.platform === "win32") {
    // 根进程已退出时子进程仍可能锁住 profile，因此无论 exitCode 如何都按本次 PID 调用一次进程树清理。
    const forced = spawn("taskkill.exe", ["/F", "/T", "/PID", String(child.pid)], { stdio: ["ignore", "ignore", "pipe"], windowsHide: true });
    let diagnostic = "";
    forced.stderr.setEncoding("utf8");
    forced.stderr.on("data", (chunk) => { diagnostic += chunk; });
    const result = await new Promise((resolve, reject) => {
      forced.once("error", reject);
      forced.once("exit", (code) => resolve(code));
    });
    if (!(await waitExit(child))) {
      // taskkill 已按整树请求；若根句柄仍未落地，再只终止这个根 PID，避免测试进程被句柄拖住。
      child.kill("SIGKILL");
      if (!(await waitExit(child, 2000))) throw new Error(`本次 Edge 进程树未在期限内退出（PID ${child.pid}）`);
    }
    // PID 已自然退出时 taskkill 可能返回“找不到”；目录删除仍会验证是否有子进程残留。
    return { result, diagnostic: diagnostic.trim() };
  }
  if (child.exitCode === null && child.signalCode === null) child.kill("SIGTERM");
  if (!(await waitExit(child))) throw new Error(`本次 Edge 进程未在期限内退出（PID ${child.pid}）`);
  return { result: 0, diagnostic: "" };
}

/** 成功必须同时满足：本次进程树已处理、根进程已退出、临时目录确实消失。 */
export async function cleanupHeadlessEdge({ edge, temporary, temporaryBase, prefix, label }) {
  try {
    const stopped = await stopTree(edge);
    if (temporary) {
      if (dirname(temporary) !== temporaryBase || !temporary.includes(prefix)) {
        throw new Error(`拒绝清理未预期的临时目录：${temporary}`);
      }
      let lastError;
      for (let attempt = 0; attempt < 40; attempt += 1) {
        try {
          rmSync(temporary, { recursive: true, force: true });
          if (!existsSync(temporary)) { lastError = null; break; }
        } catch (error) {
          lastError = error;
          if (!["EPERM", "EBUSY"].includes(error?.code)) throw error;
        }
        await pause(250);
      }
      if (lastError || existsSync(temporary)) throw lastError ?? new Error(`临时目录仍存在：${temporary}`);
    }
    console.log(`[${label} cleanup] EDGE_TREE_STOPPED TEMP_REMOVED pid=${edge?.pid ?? "none"} taskkill=${stopped?.result ?? "n/a"}`);
  } catch (error) {
    process.exitCode = 1;
    console.error(`[${label} cleanup] FAILED pid=${edge?.pid ?? "none"} temporary=${temporary ?? "none"}`, error);
    throw error;
  } finally {
    edge?.stderr?.destroy();
    edge?.unref();
  }
}
