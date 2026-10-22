<script lang="ts">
  let query = $state("");
</script>

<main>
  <input bind:value={query} placeholder="搜索剪贴板历史" />
  <p>G0 工程骨架：面板样机在这里搭建。</p>
</main>

<style>
  main {
    padding: 12px;
  }

  input {
    width: 100%;
    box-sizing: border-box;
    padding: 8px 10px;
    font: inherit;
  }
</style>
