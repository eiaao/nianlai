<script lang="ts">
  import type { SettingsPageId } from "../lib/settingsNavigation";

  let { page, ontry = () => {} }: { page: SettingsPageId; ontry?: () => void } = $props();

  const NOTES: Record<SettingsPageId, { title: string; lines: string[] }> = {
    general: {
      title: "预览就是说明",
      lines: [
        "“出现在哪”一换，预览里的面板立刻挪过去。",
        "“动效”每切一档，就演示一次摊开。",
        "呼出键录好后，这里直接显示新键。",
        "面板位置、主题和动效可以分开调。",
      ],
    },
    fav: {
      title: "常用牌怎么排",
      lines: [
        "拖动换位置，数字跟着位置走，松手就保存。",
        "点一张翻到背面写标题和备注；原内容不能改。",
        "复制、粘贴都不会改变顺序，只有自己调整顺序才会变。",
      ],
    },
    privacy: {
      title: "不记的，看得见",
      lines: [
        "排除的程序里复制的内容不记，常见的密码管理器已经放进名单。",
        "每条“像密码”的规则都配一个打码示例。",
        "判断顺序、等待时间这些细节仍可直接操作。",
      ],
    },
    data: {
      title: "东西都在这台电脑上",
      lines: [
        "占用数字和分类条一起显示，这里不会自己删东西。",
        "扔掉的先放进回收站，7 天后自动清掉。",
        "回收站里可以拿回、彻底删除或清空，仍沿用现有确认。",
      ],
    },
    habits: {
      title: "只管鼠标",
      lines: [
        "左键、中键各自一行，四个现有选项都保留。",
        "贴过的旧内容可以挪到最前，常用牌的顺序不受影响。",
        "这里改鼠标时的习惯，键盘怎么按不受影响。",
      ],
    },
    about: {
      title: "复制过的，信手拈来",
      lines: [
        "“拈”是用两三个指头轻轻拿起。",
        "版本和数据目录都来自正在运行的拈来。",
        "拈来 MIT 和第三方许可说明都能离线查看，不需要打开网页。",
        "“再看一次”从第一拍重新保存引导；“再发一遍”只把四张新手牌放回最近。",
        "如果纸笺没打开，可以只重试开窗，不会重复保存引导进度。",
      ],
    },
  };
  const note = $derived(NOTES[page]);
</script>

<aside class="settings-notes" data-settings-notes aria-label="本页说明">
  <section class="note-card">
    <h2>{note.title}</h2>
    <ul>{#each note.lines as line}<li>{line}</li>{/each}</ul>
    {#if page === "general"}<div class="tryit"><button type="button" data-try="preview" onclick={ontry}>演示一次摊开</button></div>{/if}
  </section>
  <section class="note-card rules">
    <h2>保存结果就近显示</h2>
    <ul>
      <li>改动自动保存，成功时显示“✓ 已保存”，1.5 秒后消失。</li>
      <li>读取或保存失败会写明原因，失败时不显示成功。</li>
    </ul>
  </section>
</aside>

<style>
  .settings-notes { min-width: 0; overflow: auto; padding: 24px 20px; border-left: 1px solid var(--hand-line); background: var(--hand-card-back); }
  .note-card { padding: 16px; border: 1px solid var(--hand-line); border-radius: 8px; background: var(--hand-card); box-shadow: var(--hand-row-shadow); }
  .note-card + .note-card { margin-top: 12px; }
  h2 { margin: 0 0 12px; color: var(--hand-ink); font: 600 15px var(--font-ui); }
  ul { margin: 0; padding-left: 16px; color: var(--hand-ink-2); font-size: 12px; line-height: 1.7; }
  li + li { margin-top: 8px; }
  .tryit { display: flex; flex-wrap: wrap; gap: 8px; margin-top: 16px; }
  .tryit button { min-height: 32px; padding: 4px 12px; border: 1px solid var(--hand-line-strong); border-radius: 4px; background: var(--hand-card-back); color: var(--hand-ink); font-size: 12px; }
  .tryit button:hover { border-color: var(--hand-seal); }
  .rules { border-top: 3px solid var(--hand-seal); }
</style>
