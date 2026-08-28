use std::collections::HashMap;

use crate::core::error::CoreError;

pub fn parse_header_json(raw: &str) -> Result<HashMap<String, String>, CoreError> {
    if raw.trim().is_empty() {
        return Ok(HashMap::new());
    }
    let parsed: HashMap<String, String> = serde_json::from_str(raw.trim())
        .map_err(|_| CoreError::InvalidInput("请求头必须是 JSON 对象".into()))?;
    validate_headers(parsed)
}

pub fn validate_headers(
    headers: HashMap<String, String>,
) -> Result<HashMap<String, String>, CoreError> {
    let mut normalized = HashMap::new();
    for (name, value) in headers {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err(CoreError::InvalidInput("请求头名称不能为空".into()));
        }
        if name.contains(|character: char| character.is_control() || character == ':') {
            return Err(CoreError::InvalidInput("请求头名称包含非法字符".into()));
        }
        let value = value.trim().to_string();
        if value.contains(|character: char| character.is_control()) {
            return Err(CoreError::InvalidInput(format!(
                "请求头 {name} 的值包含非法字符"
            )));
        }
        normalized.insert(name, value);
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_header_name() {
        let mut headers = HashMap::new();
        headers.insert("Bad:Header".to_string(), "value".to_string());
        assert!(validate_headers(headers).is_err());
    }

    #[test]
    fn accepts_empty_input() {
        assert!(parse_header_json("").unwrap().is_empty());
    }
}
