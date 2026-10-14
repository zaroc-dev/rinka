//! Wallpaper Engine scene values.
//!
//! A value is a number, a bool, a string of space-separated numbers
//! (`"1 0.5 0"`), or an object wrapping one of those with extra behavior:
//! `{"value": …, "user": "prop"}` (bound to a user property),
//! `{"value": …, "script": "…"}` (driven by SceneScript), or
//! `{"value": …, "animation": …}` (keyframed). The converter takes the
//! static value and reports what was dropped.

use serde_json::Value;

/// What a value carried besides its static content.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Extras {
    pub user: Option<String>,
    pub script: bool,
    pub animation: bool,
}

impl Extras {
    pub fn any(&self) -> bool {
        self.user.is_some() || self.script || self.animation
    }
}

/// Unwraps `{"value": …}` objects, returning the inner value and extras.
pub fn unwrap(v: &Value) -> (&Value, Extras) {
    match v {
        Value::Object(map) if map.contains_key("value") => {
            let user = match map.get("user") {
                Some(Value::String(s)) => Some(s.clone()),
                Some(Value::Object(o)) => o.get("name").and_then(Value::as_str).map(str::to_owned),
                _ => None,
            };
            let extras = Extras {
                user,
                script: map.contains_key("script"),
                animation: map.contains_key("animation"),
            };
            (&map["value"], extras)
        }
        other => (other, Extras::default()),
    }
}

/// Reads a value as floats: numbers, bools and number strings.
pub fn floats(v: &Value) -> Option<Vec<f32>> {
    match unwrap(v).0 {
        Value::Number(n) => Some(vec![n.as_f64()? as f32]),
        Value::Bool(b) => Some(vec![f32::from(u8::from(*b))]),
        Value::String(s) => s
            .split_whitespace()
            .map(|p| p.parse::<f32>().ok())
            .collect(),
        Value::Array(a) => a.iter().map(|x| x.as_f64().map(|f| f as f32)).collect(),
        _ => None,
    }
}

pub fn float(v: &Value) -> Option<f32> {
    floats(v).and_then(|f| f.first().copied())
}

pub fn vec_n<const N: usize>(v: &Value) -> Option<[f32; N]> {
    let f = floats(v)?;
    (f.len() >= N).then(|| std::array::from_fn(|i| f[i]))
}

pub fn boolean(v: &Value) -> Option<bool> {
    match unwrap(v).0 {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => Some(n.as_f64()? != 0.0),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_the_forms() {
        assert_eq!(floats(&json!("1 0.5 -2")), Some(vec![1.0, 0.5, -2.0]));
        assert_eq!(float(&json!(0.25)), Some(0.25));
        assert_eq!(vec_n::<2>(&json!("3 4 5")), Some([3.0, 4.0]));
        assert_eq!(boolean(&json!({"value": true, "user": "show"})), Some(true));
        let scripted = json!({"value": "1 1 1", "script": "export function update() {}"});
        let (v, extras) = unwrap(&scripted);
        assert_eq!(v, &json!("1 1 1"));
        assert!(extras.script && extras.any());
        assert_eq!(
            unwrap(&json!({"value": 1, "user": {"name": "speed", "value": 1}}))
                .1
                .user
                .as_deref(),
            Some("speed")
        );
    }
}
