pub fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

pub fn occurrences(text: &str, needle: &str) -> usize {
    if needle.is_empty() {
        0
    } else {
        text.matches(needle).count()
    }
}

pub fn increased(before_count: usize, after: &str, pasted: &str) -> bool {
    let pasted = normalize_newlines(pasted);
    !pasted.is_empty() && occurrences(&normalize_newlines(after), &pasted) > before_count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_occurrences_and_normalizes_all_newlines() {
        let compare = |before: &str, after: &str, pasted: &str| {
            let count = occurrences(&normalize_newlines(before), &normalize_newlines(pasted));
            increased(count, after, pasted)
        };
        assert!(compare(
            "已有\r\n文字",
            "已有\r\n文字\r新增\r\n两行",
            "新增\n两行"
        ));
        assert!(compare("你好", "你好你好", "你好"));
        assert!(!compare("你好", "你好", "你好"));
        assert!(!compare("你好你好", "你好", "你好"));
        assert!(!compare("x", "y", ""));
        assert!(!compare("无", "其他", "目标"));
        assert!(!compare("a\r\nb", "a\nb", "a\nb"));
    }
}
