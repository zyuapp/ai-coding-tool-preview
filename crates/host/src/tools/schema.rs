//! A tool's input, described once: it writes the JSON Schema an MCP client reads, and checks and
//! normalizes the arguments a call brings the way the TypeScript app's zod shapes did.

use serde_json::{json, Map, Value};

/// A check a value must pass beyond its shape, and what a refusal says.
pub type TextCheck = (fn(&str) -> bool, &'static str);
pub type ItemsCheck = (fn(&[Value]) -> bool, &'static str);

#[derive(Clone)]
pub enum Kind {
    String {
        min: Option<usize>,
        max: Option<usize>,
        trim: bool,
        transform: Option<fn(String) -> String>,
        refine: Option<TextCheck>,
    },
    Number {
        min: Option<f64>,
        max: Option<f64>,
        integer: bool,
    },
    Boolean,
    Enum(Vec<&'static str>),
    Array {
        item: Box<Field>,
        min: Option<usize>,
        max: Option<usize>,
        refine: Option<ItemsCheck>,
    },
    Object(Vec<(&'static str, Field)>),
}

#[derive(Clone)]
pub struct Field {
    pub kind: Kind,
    pub optional: bool,
    pub nullable: bool,
    pub description: Option<String>,
}

pub fn string() -> Field {
    Field::of(Kind::String { min: None, max: None, trim: false, transform: None, refine: None })
}

pub fn number() -> Field {
    Field::of(Kind::Number { min: None, max: None, integer: false })
}

pub fn integer() -> Field {
    Field::of(Kind::Number { min: None, max: None, integer: true })
}

pub fn boolean() -> Field {
    Field::of(Kind::Boolean)
}

pub fn enumeration(values: &[&'static str]) -> Field {
    Field::of(Kind::Enum(values.to_vec()))
}

pub fn array(item: Field) -> Field {
    Field::of(Kind::Array { item: Box::new(item), min: None, max: None, refine: None })
}

pub fn object(fields: Vec<(&'static str, Field)>) -> Field {
    Field::of(Kind::Object(fields))
}

impl Field {
    fn of(kind: Kind) -> Self {
        Self { kind, optional: false, nullable: false, description: None }
    }

    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }

    pub fn nullable(mut self) -> Self {
        self.nullable = true;
        self
    }

    pub fn describe(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// The least: characters for a string, items for an array, value for a number.
    pub fn min(mut self, least: usize) -> Self {
        match &mut self.kind {
            Kind::String { min, .. } | Kind::Array { min, .. } => *min = Some(least),
            Kind::Number { min, .. } => *min = Some(least as f64),
            _ => {}
        }
        self
    }

    pub fn max(mut self, most: usize) -> Self {
        match &mut self.kind {
            Kind::String { max, .. } | Kind::Array { max, .. } => *max = Some(most),
            Kind::Number { max, .. } => *max = Some(most as f64),
            _ => {}
        }
        self
    }

    pub fn nonnegative(self) -> Self {
        self.min(0)
    }

    /// Trimmed before any length is checked, as zod's `.trim()` does.
    pub fn trim(mut self) -> Self {
        if let Kind::String { trim, .. } = &mut self.kind {
            *trim = true;
        }
        self
    }

    pub fn transform(mut self, change: fn(String) -> String) -> Self {
        if let Kind::String { transform, .. } = &mut self.kind {
            *transform = Some(change);
        }
        self
    }

    pub fn refine(mut self, check: fn(&str) -> bool, message: &'static str) -> Self {
        if let Kind::String { refine, .. } = &mut self.kind {
            *refine = Some((check, message));
        }
        self
    }

    pub fn refine_items(mut self, check: fn(&[Value]) -> bool, message: &'static str) -> Self {
        if let Kind::Array { refine, .. } = &mut self.kind {
            *refine = Some((check, message));
        }
        self
    }

    /// The draft-07 JSON Schema of this field.
    pub fn json_schema(&self) -> Value {
        let mut schema = Map::new();
        match &self.kind {
            Kind::String { min, max, .. } => {
                schema.insert("type".into(), json!("string"));
                if let Some(min) = min {
                    schema.insert("minLength".into(), json!(min));
                }
                if let Some(max) = max {
                    schema.insert("maxLength".into(), json!(max));
                }
            }
            Kind::Number { min, max, integer } => {
                schema.insert("type".into(), json!(if *integer { "integer" } else { "number" }));
                if let Some(min) = min {
                    schema.insert("minimum".into(), json!(min));
                } else if *integer {
                    schema.insert("minimum".into(), json!(-9_007_199_254_740_991i64));
                }
                if let Some(max) = max {
                    schema.insert("maximum".into(), json!(max));
                } else if *integer {
                    schema.insert("maximum".into(), json!(9_007_199_254_740_991i64));
                }
            }
            Kind::Boolean => {
                schema.insert("type".into(), json!("boolean"));
            }
            Kind::Enum(values) => {
                schema.insert("type".into(), json!("string"));
                schema.insert("enum".into(), json!(values));
            }
            Kind::Array { item, min, max, .. } => {
                schema.insert("type".into(), json!("array"));
                schema.insert("items".into(), item.json_schema());
                if let Some(min) = min {
                    schema.insert("minItems".into(), json!(min));
                }
                if let Some(max) = max {
                    schema.insert("maxItems".into(), json!(max));
                }
            }
            Kind::Object(fields) => {
                schema.insert("type".into(), json!("object"));
                let properties: Map<String, Value> = fields.iter().map(|(name, field)| (name.to_string(), field.json_schema())).collect();
                schema.insert("properties".into(), Value::Object(properties));
                let required: Vec<&str> = fields.iter().filter(|(_, field)| !field.optional).map(|(name, _)| *name).collect();
                if !required.is_empty() {
                    schema.insert("required".into(), json!(required));
                }
                schema.insert("additionalProperties".into(), json!(false));
            }
        }
        let mut schema = if self.nullable { json!({ "anyOf": [Value::Object(schema), { "type": "null" }] }) } else { Value::Object(schema) };
        if let (Some(description), Some(object)) = (&self.description, schema.as_object_mut()) {
            object.insert("description".into(), json!(description));
        }
        schema
    }

    /// Checks a value, answering with it normalized: strings trimmed and transformed, unknown keys dropped.
    pub fn parse(&self, value: Option<&Value>, path: &mut Vec<String>, issues: &mut Vec<Value>) -> Option<Value> {
        let Some(value) = value else {
            if !self.optional {
                issues.push(issue(path, "invalid_type", &format!("Invalid input: expected {}, received undefined", self.expected())));
            }
            return None;
        };
        if value.is_null() && self.nullable {
            return Some(Value::Null);
        }
        match &self.kind {
            Kind::String { min, max, trim, transform, refine } => {
                let Some(text) = value.as_str() else { return self.mismatch(value, path, issues) };
                let mut text = if *trim { text.trim().to_string() } else { text.to_string() };
                let length = text.encode_utf16().count();
                if let Some(min) = min.filter(|min| length < *min) {
                    issues.push(issue(path, "too_small", &format!("Too small: expected string to have >={min} characters")));
                    return None;
                }
                if let Some(max) = max.filter(|max| length > *max) {
                    issues.push(issue(path, "too_big", &format!("Too big: expected string to have <={max} characters")));
                    return None;
                }
                if let Some(transform) = transform {
                    text = transform(text);
                }
                if let Some((check, message)) = refine {
                    if !check(&text) {
                        issues.push(issue(path, "custom", message));
                        return None;
                    }
                }
                Some(Value::String(text))
            }
            Kind::Number { min, max, integer } => {
                let Some(number) = value.as_f64().filter(|number| number.is_finite()) else { return self.mismatch(value, path, issues) };
                if *integer && number.fract() != 0.0 {
                    issues.push(issue(path, "invalid_type", "Invalid input: expected int, received number"));
                    return None;
                }
                if let Some(min) = min.filter(|min| number < *min) {
                    issues.push(issue(path, "too_small", &format!("Too small: expected number to be >={min}")));
                    return None;
                }
                if let Some(max) = max.filter(|max| number > *max) {
                    issues.push(issue(path, "too_big", &format!("Too big: expected number to be <={max}")));
                    return None;
                }
                Some(value.clone())
            }
            Kind::Boolean => value.is_boolean().then(|| value.clone()).or_else(|| self.mismatch(value, path, issues)),
            Kind::Enum(values) => match value.as_str() {
                Some(text) if values.contains(&text) => Some(value.clone()),
                _ => {
                    let options = values.iter().map(|value| format!("\"{value}\"")).collect::<Vec<_>>().join("|");
                    issues.push(issue(path, "invalid_value", &format!("Invalid option: expected one of {options}")));
                    None
                }
            },
            Kind::Array { item, min, max, refine } => {
                let Some(items) = value.as_array() else { return self.mismatch(value, path, issues) };
                let before = issues.len();
                let mut parsed = Vec::with_capacity(items.len());
                for (index, entry) in items.iter().enumerate() {
                    path.push(index.to_string());
                    if let Some(entry) = item.parse(Some(entry), path, issues) {
                        parsed.push(entry);
                    }
                    path.pop();
                }
                if let Some(min) = min.filter(|min| items.len() < *min) {
                    issues.push(issue(path, "too_small", &format!("Too small: expected array to have >={min} items")));
                }
                if let Some(max) = max.filter(|max| items.len() > *max) {
                    issues.push(issue(path, "too_big", &format!("Too big: expected array to have <={max} items")));
                }
                if issues.len() > before {
                    return None;
                }
                if let Some((check, message)) = refine {
                    if !check(&parsed) {
                        issues.push(issue(path, "custom", message));
                        return None;
                    }
                }
                Some(Value::Array(parsed))
            }
            Kind::Object(fields) => {
                let Some(given) = value.as_object() else { return self.mismatch(value, path, issues) };
                let mut parsed = Map::new();
                for (name, field) in fields {
                    path.push(name.to_string());
                    if let Some(value) = field.parse(given.get(*name), path, issues) {
                        parsed.insert(name.to_string(), value);
                    }
                    path.pop();
                }
                Some(Value::Object(parsed))
            }
        }
    }

    fn expected(&self) -> &'static str {
        match &self.kind {
            Kind::String { .. } | Kind::Enum(_) => "string",
            Kind::Number { .. } => "number",
            Kind::Boolean => "boolean",
            Kind::Array { .. } => "array",
            Kind::Object(_) => "object",
        }
    }

    fn mismatch(&self, value: &Value, path: &[String], issues: &mut Vec<Value>) -> Option<Value> {
        let received = match value {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        };
        issues.push(issue(path, "invalid_type", &format!("Invalid input: expected {}, received {received}", self.expected())));
        None
    }
}

fn issue(path: &[String], code: &str, message: &str) -> Value {
    let path: Vec<Value> = path.iter().map(|part| part.parse::<u64>().map(Value::from).unwrap_or_else(|_| Value::String(part.clone()))).collect();
    json!({ "code": code, "path": path, "message": message })
}

/// A tool's whole input: named fields of one object.
#[derive(Clone)]
pub struct Input {
    pub fields: Vec<(&'static str, Field)>,
}

impl Input {
    pub fn new(fields: Vec<(&'static str, Field)>) -> Self {
        Self { fields }
    }

    pub fn empty() -> Self {
        Self { fields: Vec::new() }
    }

    pub fn with(mut self, name: &'static str, field: Field) -> Self {
        match self.fields.iter_mut().find(|(known, _)| *known == name) {
            Some(existing) => existing.1 = field,
            None => self.fields.push((name, field)),
        }
        self
    }

    /// The schema an MCP client is told the tool takes.
    pub fn json_schema(&self) -> Map<String, Value> {
        let Value::Object(mut schema) = object(self.fields.clone()).json_schema() else { unreachable!() };
        schema.insert("$schema".into(), json!("http://json-schema.org/draft-07/schema#"));
        schema
    }

    /// The arguments as the tool reads them, or the issues as zod would list them.
    pub fn parse(&self, arguments: &Map<String, Value>) -> Result<Map<String, Value>, String> {
        let mut issues = Vec::new();
        let parsed = object(self.fields.clone()).parse(Some(&Value::Object(arguments.clone())), &mut Vec::new(), &mut issues);
        match parsed {
            Some(Value::Object(map)) if issues.is_empty() => Ok(map),
            _ => Err(serde_json::to_string_pretty(&issues).unwrap_or_default()),
        }
    }
}

/// Typed reads of parsed arguments.
pub trait Args {
    fn text(&self, name: &str) -> Option<String>;
    fn flag(&self, name: &str) -> Option<bool>;
    fn float(&self, name: &str) -> Option<f64>;
}

impl Args for Map<String, Value> {
    fn text(&self, name: &str) -> Option<String> {
        self.get(name).and_then(Value::as_str).map(String::from)
    }

    fn flag(&self, name: &str) -> Option<bool> {
        self.get(name).and_then(Value::as_bool)
    }

    fn float(&self, name: &str) -> Option<f64> {
        self.get(name).and_then(Value::as_f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> Input {
        Input::new(vec![
            ("prompt", string().trim().min(1).max(10).describe("What to do.")),
            ("limit", number().nonnegative().optional()),
            ("role", enumeration(&["a", "b"]).nullable()),
        ])
    }

    #[test]
    fn writes_draft_seven_with_required_fields() {
        let schema = Value::Object(input().json_schema());
        assert_eq!(schema["required"], json!(["prompt", "role"]));
        assert_eq!(schema["properties"]["prompt"], json!({ "type": "string", "minLength": 1, "maxLength": 10, "description": "What to do." }));
        assert_eq!(schema["properties"]["role"]["anyOf"][1], json!({ "type": "null" }));
    }

    #[test]
    fn trims_before_measuring_and_drops_unknown_keys() {
        let arguments = json!({ "prompt": "  go  ", "role": null, "extra": 1 });
        let parsed = input().parse(arguments.as_object().unwrap()).unwrap();
        assert_eq!(Value::Object(parsed), json!({ "prompt": "go", "role": null }));
        let blank = json!({ "prompt": "   ", "role": "a" });
        assert!(input().parse(blank.as_object().unwrap()).unwrap_err().contains("\"prompt\""));
        let negative = json!({ "prompt": "x", "role": "c", "limit": -1 });
        let issues = input().parse(negative.as_object().unwrap()).unwrap_err();
        assert!(issues.contains("limit") && issues.contains("role"));
    }
}
