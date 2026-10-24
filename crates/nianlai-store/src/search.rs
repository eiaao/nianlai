use crate::*;
use rusqlite::params_from_iter;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    Recent,
    Index,
    Substring,
    Mixed,
}
#[derive(Serialize)]
pub struct Search {
    pub items: Vec<Metadata>,
    pub words: usize,
    pub characters: usize,
    pub mode: SearchMode,
    pub elapsed_ms: f64,
}
pub fn search(c: &Connection, query: &str) -> Result<Search> {
    let started = Instant::now();
    let words: Vec<_> = query.split_whitespace().collect();
    let mut indexed = Vec::new();
    let mut short = Vec::new();
    for word in &words {
        if word.chars().count() >= 3 {
            indexed.push(format!("\"{}\"", word.replace('"', "\"\"")));
        } else {
            short.push(word.to_ascii_lowercase());
        }
    }
    let mode = match (indexed.is_empty(), short.is_empty()) {
        (true, true) => SearchMode::Recent,
        (false, true) => SearchMode::Index,
        (true, false) => SearchMode::Substring,
        (false, false) => SearchMode::Mixed,
    };
    // 常见词可能命中半个库；沿排序索引取满 300 条即停止，避免先搬出全部预览再排序。
    let mut sql = format!(
        "SELECT {} FROM contents c INDEXED BY content_order",
        schema::COLUMNS
    );
    let mut predicates = Vec::new();
    let mut params = Vec::new();
    if !indexed.is_empty() {
        predicates
            .push("c.id IN (SELECT rowid FROM search_index WHERE search_index MATCH ?)".to_owned());
        params.push(indexed.join(" AND "));
    }
    for word in short {
        // instr 不把 %、_ 和反斜杠当成通配符；正文在写入时只折叠 ASCII 大小写。
        predicates.push("instr(c.search_text,?) > 0".to_owned());
        params.push(word);
    }
    if !predicates.is_empty() {
        sql.push_str(&format!(" WHERE {}", predicates.join(" AND ")));
    }
    sql.push_str(" ORDER BY c.sort_ms DESC,c.sort_order DESC LIMIT 300");
    let items = c
        .prepare_cached(&sql)?
        .query_map(params_from_iter(params), schema::metadata_row)?
        .collect::<std::result::Result<_, _>>()?;
    Ok(Search {
        items,
        words: words.len(),
        characters: query.chars().count(),
        mode,
        elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
    })
}
