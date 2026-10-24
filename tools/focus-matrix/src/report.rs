use crate::{
    cli::Mode,
    judge::{self, Cell, Class, Trial},
    scenario::Scenario,
};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Serialize)]
pub struct Detail {
    pub id: String,
    pub name: String,
    pub row: u8,
    pub mode: Mode,
    pub requirement: crate::scenario::Requirement,
    pub planned: u32,
    pub valid: usize,
    pub counts: BTreeMap<Class, usize>,
    pub verdict: Cell,
    pub hotkey_median_ms: Option<f64>,
    pub hotkey_p95_ms: Option<f64>,
    pub result_median_ms: Option<f64>,
    pub result_p95_ms: Option<f64>,
}
#[derive(Serialize)]
pub struct MatrixRow {
    pub row: u8,
    pub quick: Cell,
    pub search: Cell,
}
#[derive(Serialize)]
pub struct Summary {
    pub matrix: Vec<MatrixRow>,
    pub scenarios: Vec<Detail>,
    pub success: bool,
}
pub fn percentile(values: impl Iterator<Item = f64>, percent: f64) -> Option<f64> {
    let mut v: Vec<_> = values.filter(|v| v.is_finite()).collect();
    v.sort_by(f64::total_cmp);
    if v.is_empty() {
        None
    } else {
        Some(
            v[((v.len() as f64 * percent).ceil() as usize)
                .saturating_sub(1)
                .min(v.len() - 1)],
        )
    }
}
impl Summary {
    pub fn build(scenarios: &[&Scenario], modes: &[Mode], n: u32, trials: &[Trial]) -> Self {
        let mut details = vec![];
        for s in scenarios {
            for &mode in modes {
                let rows: Vec<_> = trials
                    .iter()
                    .filter(|r| r.scenario == s.id && r.mode == mode && r.trial > 0)
                    .collect();
                let mut counts = BTreeMap::new();
                for r in &rows {
                    *counts.entry(r.result.class).or_insert(0) += 1;
                }
                details.push(Detail {
                    id: s.id.clone(),
                    name: s.name.clone(),
                    row: s.row,
                    mode,
                    requirement: s.requirement(mode),
                    planned: n,
                    valid: rows.iter().filter(|r| r.result.class.valid()).count(),
                    counts,
                    verdict: judge::cell(s.requirement(mode), n, &rows),
                    hotkey_median_ms: percentile(rows.iter().filter_map(|r| r.hotkey_ms), 0.5),
                    hotkey_p95_ms: percentile(rows.iter().filter_map(|r| r.hotkey_ms), 0.95),
                    result_median_ms: percentile(rows.iter().filter_map(|r| r.result_ms), 0.5),
                    result_p95_ms: percentile(rows.iter().filter_map(|r| r.result_ms), 0.95),
                });
            }
        }
        let matrix = (1..=8)
            .map(|row| {
                let fold = |mode| {
                    let cells: Vec<_> = details
                        .iter()
                        .filter(|s| s.row == row && s.mode == mode)
                        .map(|s| s.verdict)
                        .collect();
                    if cells.is_empty() {
                        Cell::Manual
                    } else if cells.contains(&Cell::Fail) {
                        Cell::Fail
                    } else if cells.contains(&Cell::Incomplete) {
                        Cell::Incomplete
                    } else {
                        Cell::Pass
                    }
                };
                MatrixRow {
                    row,
                    quick: fold(Mode::Quick),
                    search: fold(Mode::Search),
                }
            })
            .collect();
        let success = !details.is_empty() && details.iter().all(|s| s.verdict == Cell::Pass);
        Self {
            matrix,
            scenarios: details,
            success,
        }
    }
    pub fn markdown(&self) -> String {
        let mut text = String::from(
            "# 拈来 G0 焦点矩阵\n\n仅表示本次选中自动场景的覆盖；手工格、未选中的场景和独占全屏游戏仍需补验。\n\n| PRD 行 | 快速取用 | 搜索模式 |\n|---|---|---|\n",
        );
        for row in &self.matrix {
            text += &format!(
                "| {} | {} | {} |\n",
                row.row,
                row.quick.label(),
                row.search.label()
            );
        }
        text += "\n| 场景 | 模式 | 结论 | 有效/要求 | 分类计数 | 呼出中位/P95 ms | 结果中位/P95 ms |\n|---|---|---|---|---|---|---|\n";
        let number = |v: Option<f64>| v.map(|n| format!("{n:.1}")).unwrap_or_else(|| "—".into());
        for s in &self.scenarios {
            text += &format!(
                "| {} | {} | {} | {}/{} | {:?} | {}/{} | {}/{} |\n",
                s.name,
                s.mode.label(),
                s.verdict.label(),
                s.valid,
                s.planned,
                s.counts,
                number(s.hotkey_median_ms),
                number(s.hotkey_p95_ms),
                number(s.result_median_ms),
                number(s.result_p95_ms)
            );
        }
        text += "\n有效次数不包含 harness_error 和 skipped；不足为未完成。必须通过要求全部 pass；尽力要求只有 pass 或有充分证据的 degraded。trial=0 是预热，不计入验收。多种异常保存在 flags，不能只看主分类。\n";
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn table_all_eight_rows() {
        let c: crate::scenario::Config = serde_json::from_str(crate::scenario::DEFAULT).unwrap();
        let s = Summary::build(&[&c.scenarios[0]], &[Mode::Quick, Mode::Search], 30, &[]);
        assert_eq!(s.matrix.len(), 8);
        assert_eq!(s.matrix[0].quick, Cell::Incomplete);
        assert_eq!(s.matrix[3].quick, Cell::Manual);
        assert!(s.markdown().contains("| 8 | 手工 | 手工 |"));
        assert!(!s.success);
    }
    #[test]
    fn statistics() {
        assert_eq!(percentile((1..=20).map(f64::from), 0.95), Some(19.0));
        assert_eq!(percentile([].into_iter(), 0.5), None);
    }
}
