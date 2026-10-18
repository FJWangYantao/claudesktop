//! JSON 解析错误的中文说明：手写配置文件（mcp.json、插件清单）最常见的几种错，
//! 说清楚在第几行第几列、大概是什么问题；认不出的原样附上 serde 的原文。

/// `serde_json` 的错误 → 「第 3 行第 5 列：最后一项后面多了逗号」。
pub fn json_error_text(e: &serde_json::Error) -> String {
    let raw = e.to_string();
    // serde 的原文末尾带「 at line N column M」，中文说明里单独写位置
    let msg = raw.split(" at line ").next().unwrap_or(&raw);
    let what = if e.is_eof() {
        "文件没写完：括号或引号没有配对".to_string()
    } else if msg.starts_with("trailing comma") {
        "最后一项后面多了逗号".into()
    } else if msg.starts_with("expected `,` or `}`") || msg.starts_with("expected `,` or `]`") {
        "少了逗号，或者括号没有配对".into()
    } else if msg.starts_with("key must be a string") {
        "键名要用双引号括起来".into()
    } else if msg.starts_with("expected `:`") {
        "键名后面少了冒号".into()
    } else if msg.starts_with("expected value") {
        "这里缺一个值（字符串要用双引号，JSON 里不能写注释）".into()
    } else if msg.starts_with("trailing characters") {
        "结尾多出了内容（整个文件只能是一个 {…}）".into()
    } else if msg.starts_with("invalid escape") {
        "反斜杠要写成 \\\\（Windows 路径也可以改用 /）".into()
    } else if msg.starts_with("control character") {
        "字符串里不能直接换行".into()
    } else {
        msg.to_string()
    };
    if e.line() == 0 {
        what
    } else {
        format!("第 {} 行第 {} 列：{what}", e.line(), e.column())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(json: &str) -> String {
        json_error_text(&serde_json::from_str::<serde_json::Value>(json).unwrap_err())
    }

    #[test]
    fn explains_common_mistakes_with_a_position() {
        assert_eq!(text("{\"a\": 1,\n}"), "第 2 行第 1 列：最后一项后面多了逗号");
        assert_eq!(text("{\"a\": 1 \"b\": 2}"), "第 1 行第 9 列：少了逗号，或者括号没有配对");
        assert_eq!(text("{a: 1}"), "第 1 行第 2 列：键名要用双引号括起来");
        assert!(text("{\"a\": {").ends_with("文件没写完：括号或引号没有配对"));
        assert!(
            text("{\"p\": \"C:\\x\"}").ends_with("反斜杠要写成 \\\\（Windows 路径也可以改用 /）")
        );
    }
}
