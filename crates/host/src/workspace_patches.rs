//! A published workspace state as JSON patches: only changed branches cross to another computer, and
//! the computer holding a replica puts them back.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSplice {
    pub index: usize,
    pub delete_count: usize,
    pub items: Vec<Value>,
}

/// One change, addressed by its path into the record: a value set or removed, or an array spliced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WorkspacePatch {
    Splice { path: Vec<Value>, splice: WorkspaceSplice },
    Set {
        path: Vec<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remove: Option<bool>,
    },
}

/// The patches that turn `previous` into `next`. Equal branches cost nothing.
pub fn workspace_patches(previous: &Value, next: &Value) -> Vec<WorkspacePatch> {
    let mut patches = Vec::new();
    diff(previous, next, &mut Vec::new(), &mut patches);
    patches
}

fn diff(previous: &Value, next: &Value, path: &mut Vec<Value>, patches: &mut Vec<WorkspacePatch>) {
    if previous == next {
        return;
    }
    match (previous, next) {
        (Value::Array(before), Value::Array(after)) => {
            let mut prefix = 0;
            while prefix < before.len() && prefix < after.len() && before[prefix] == after[prefix] {
                prefix += 1;
            }
            let mut suffix = 0;
            while suffix < before.len() - prefix && suffix < after.len() - prefix && before[before.len() - suffix - 1] == after[after.len() - suffix - 1] {
                suffix += 1;
            }
            if before.len() != after.len() {
                patches.push(WorkspacePatch::Splice { path: path.clone(), splice: WorkspaceSplice { index: prefix, delete_count: before.len() - prefix - suffix, items: after[prefix..after.len() - suffix].to_vec() } });
                return;
            }
            for index in prefix..after.len() - suffix {
                path.push(Value::from(index));
                diff(&before[index], &after[index], path, patches);
                path.pop();
            }
        }
        (Value::Object(before), Value::Object(after)) => {
            for (key, value) in after {
                path.push(Value::String(key.clone()));
                match before.get(key) {
                    None => patches.push(WorkspacePatch::Set { path: path.clone(), value: Some(value.clone()), remove: None }),
                    Some(held) => diff(held, value, path, patches),
                }
                path.pop();
            }
            for key in before.keys() {
                if !after.contains_key(key) {
                    let mut removed = path.clone();
                    removed.push(Value::String(key.clone()));
                    patches.push(WorkspacePatch::Set { path: removed, value: None, remove: Some(true) });
                }
            }
        }
        _ => patches.push(WorkspacePatch::Set { path: path.clone(), value: Some(next.clone()), remove: None }),
    }
}

fn child<'a>(parent: &'a mut Value, segment: &Value) -> Result<&'a mut Value, String> {
    match (parent, segment) {
        (Value::Object(fields), Value::String(key)) if key != "__proto__" => fields.get_mut(key).ok_or_else(|| "Invalid workspace patch key.".to_string()),
        (Value::Array(items), Value::Number(index)) => index.as_u64().and_then(|index| items.get_mut(index as usize)).ok_or_else(|| "Invalid workspace patch key.".to_string()),
        _ => Err("Invalid workspace patch path.".into()),
    }
}

fn splice(target: &mut Value, operation: &WorkspaceSplice) -> Result<(), String> {
    let Value::Array(items) = target else { return Err("Invalid workspace array splice.".into()) };
    if operation.index > items.len() || operation.delete_count > items.len() - operation.index {
        return Err("Invalid workspace array splice.".into());
    }
    items.splice(operation.index..operation.index + operation.delete_count, operation.items.iter().cloned());
    Ok(())
}

/// Applies patches in order. A patch that names a path the state does not have is an error, which a
/// replica answers by asking for the state whole.
pub fn apply_workspace_patches(state: &mut Value, patches: &[WorkspacePatch]) -> Result<(), String> {
    for patch in patches {
        let path = match patch {
            WorkspacePatch::Splice { path, .. } | WorkspacePatch::Set { path, .. } => path,
        };
        let Some((last, parents)) = path.split_last() else {
            match patch {
                WorkspacePatch::Splice { splice: operation, .. } => splice(state, operation)?,
                WorkspacePatch::Set { value, .. } => *state = value.clone().unwrap_or(Value::Null),
            }
            continue;
        };
        let mut parent = &mut *state;
        for segment in parents {
            parent = child(parent, segment)?;
        }
        match patch {
            WorkspacePatch::Splice { splice: operation, .. } => splice(child(parent, last)?, operation)?,
            WorkspacePatch::Set { value, remove, .. } => match (parent, last) {
                (Value::Object(fields), Value::String(key)) if key != "__proto__" => {
                    if remove == &Some(true) {
                        fields.remove(key);
                    } else {
                        fields.insert(key.clone(), value.clone().unwrap_or(Value::Null));
                    }
                }
                (Value::Array(items), Value::Number(index)) => {
                    let index = index.as_u64().ok_or("Invalid workspace patch key.")? as usize;
                    if remove == &Some(true) {
                        if index < items.len() {
                            items[index] = Value::Null;
                        }
                    } else if index < items.len() {
                        items[index] = value.clone().unwrap_or(Value::Null);
                    } else if index == items.len() {
                        items.push(value.clone().unwrap_or(Value::Null));
                    } else {
                        return Err("Invalid workspace patch key.".into());
                    }
                }
                _ => return Err("Invalid workspace patch path.".into()),
            },
        }
    }
    Ok(())
}

/// JSON has no sets, so another build may send one as `{"$set": [...]}`; it reads here as the list it holds.
pub fn decode_sets(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            if fields.len() == 1 {
                if let Some(Value::Array(items)) = fields.get("$set") {
                    *value = Value::Array(items.clone());
                    if let Value::Array(items) = value {
                        items.iter_mut().for_each(decode_sets);
                    }
                    return;
                }
            }
            fields.values_mut().for_each(decode_sets);
        }
        Value::Array(items) => items.iter_mut().for_each(decode_sets),
        _ => {}
    }
}

/// A workspace as another computer is handed it: without the states of computers this one pairs with.
pub fn handed_over_state(mut state: Value) -> Value {
    if let Some(paired) = state.pointer_mut("/computers/paired").and_then(Value::as_array_mut) {
        for computer in paired {
            if let Some(fields) = computer.as_object_mut() {
                fields.insert("state".into(), Value::Null);
            }
        }
    }
    state
}

/// Patches for another computer, minus any about this one's paired computers.
pub fn handed_over_patches(patches: Vec<WorkspacePatch>) -> Vec<WorkspacePatch> {
    patches
        .into_iter()
        .filter(|patch| {
            let path = match patch {
                WorkspacePatch::Splice { path, .. } | WorkspacePatch::Set { path, .. } => path,
            };
            path.first().and_then(Value::as_str) != Some("computers")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn patches_rebuild_the_next_state() {
        let before = json!({ "threads": [{ "id": "a", "title": "A" }, { "id": "b" }], "gone": 1, "current": "a" });
        let after = json!({ "threads": [{ "id": "a", "title": "A2" }, { "id": "b" }, { "id": "c" }], "current": "b", "new": [1] });
        let patches = workspace_patches(&before, &after);
        let mut replica = before.clone();
        apply_workspace_patches(&mut replica, &patches).unwrap();
        assert_eq!(replica, after);
        let same_length = json!({ "threads": [{ "id": "a", "title": "A3" }, { "id": "b" }] });
        let mut replica = json!({ "threads": [{ "id": "a", "title": "A" }, { "id": "b" }] });
        let patches = workspace_patches(&replica, &same_length);
        apply_workspace_patches(&mut replica, &patches).unwrap();
        assert_eq!(replica, same_length);
        assert!(apply_workspace_patches(&mut json!({}), &[WorkspacePatch::Set { path: vec![json!("a"), json!("b")], value: Some(json!(1)), remove: None }]).is_err());
    }
}
