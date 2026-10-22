use serde::Serialize;

#[derive(Clone, Serialize)]
pub struct Sample {
    pub id: usize,
    pub text: String,
}

pub fn all() -> Vec<Sample> {
    let mut texts = vec![
        "你好，世界！Hello, world!".to_owned(),
        "第一行：拈来\nSecond line: clipboard panel\n第三行：保留换行与缩进".to_owned(),
        "今天也要开心 🌱 🦫 🚀 👩‍💻 🇨🇳".to_owned(),
        "https://example.com/search?q=nianlai".to_owned(),
        "fn main() {\n    let greeting = \"你好，Rust!\";\n    println!(\"{greeting}\");\n}".to_owned(),
        "这是一段用于检查长文本预览、搜索与粘贴完整性的示例。Windows 面板应保留原输入框的焦点，快速取用时不打断重命名或地址栏编辑。切换到搜索模式后，中文输入法应正常工作。The complete text must remain intact when copied. ".repeat(5),
        "待办清单\n- Review 焦点恢复\n- 检查输入法首字\n- 记录 memory tier".to_owned(),
        "Email 示例：hello@example.com".to_owned(),
        "SELECT id, text FROM samples WHERE id < 10;".to_owned(),
        "中文标点：，。！？； English punctuation: ,.!?;".to_owned(),
    ];
    let topics = [
        "设计 Design",
        "开发 Development",
        "阅读 Reading",
        "会议 Meeting",
        "旅行 Travel",
        "音乐 Music",
        "笔记 Notes",
        "灵感 Ideas",
    ];
    for n in 11..=50 {
        texts.push(format!(
            "示例 {n:02} · {} — 这是第 {n} 条剪贴板文本，sample item {n}。",
            topics[(n - 11) % topics.len()]
        ));
    }
    texts
        .into_iter()
        .enumerate()
        .map(|(index, text)| Sample {
            id: index + 1,
            text,
        })
        .collect()
}
