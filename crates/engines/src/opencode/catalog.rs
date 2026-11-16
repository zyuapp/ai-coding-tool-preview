//! OpenCode names its models itself: each connected provider's, as `provider/model` ids.

use serde_json::{json, Value};

use crate::catalog::{efforts, is_model_id};
use crate::types::{AgentEffort, ModelSpec};

/// An app model id is the provider and the provider's id joined, as OpenCode writes it.
pub fn app_model_id(model: &Value) -> String {
    format!(
        "{}/{}",
        model["providerID"].as_str().unwrap_or_default(),
        model["id"].as_str().unwrap_or_default()
    )
}

/// The server's reference for an app model id, at a variant when the model reasons at several depths.
pub fn model_ref(model: &str, variant: Option<&str>) -> Option<Value> {
    let slash = model.find('/')?;
    if slash == 0 || slash == model.len() - 1 {
        return None;
    }
    let mut reference = json!({ "providerID": &model[..slash], "id": &model[slash + 1..] });
    if let Some(variant) = variant.filter(|variant| !variant.is_empty()) {
        reference["variant"] = json!(variant);
    }
    Some(reference)
}

/// The models the server offers, as the picker lists them, the server's default first. A model
/// reasons at the depths its variants name that the app also names.
pub fn open_code_catalog(
    models: &[Value],
    providers: &[Value],
    default_model: Option<&Value>,
) -> Vec<ModelSpec> {
    let names = |provider: &str| {
        providers
            .iter()
            .find(|known| known["id"] == provider)
            .and_then(|known| known["name"].as_str())
            .unwrap_or(provider)
            .to_string()
    };
    let mut specs: Vec<ModelSpec> = models
        .iter()
        .filter(|model| model["enabled"] != false)
        .filter_map(|model| {
            let id = app_model_id(model);
            if !is_model_id(&id) {
                return None;
            }
            let offered: Vec<AgentEffort> = model["variants"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|variant| variant["id"].as_str().and_then(AgentEffort::parse))
                .collect();
            Some(ModelSpec {
                id,
                label: model["name"]
                    .as_str()
                    .or(model["id"].as_str())
                    .unwrap_or_default()
                    .to_string(),
                description: model["id"].as_str().unwrap_or_default().to_string(),
                context_window: model["limit"]["context"].as_u64().unwrap_or(0),
                efforts: efforts(&offered),
                manual_compaction: true,
                provider: Some(names(model["providerID"].as_str().unwrap_or_default())),
            })
        })
        .collect();
    if let Some(first) = default_model
        .filter(|model| model.is_object())
        .map(app_model_id)
    {
        if let Some(index) = specs
            .iter()
            .position(|spec| spec.id == first)
            .filter(|index| *index > 0)
        {
            let spec = specs.remove(index);
            specs.insert(0, spec);
        }
    }
    specs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_the_default_first_with_known_efforts() {
        let models = vec![
            json!({ "id": "a", "providerID": "p", "name": "A", "variants": [{ "id": "none" }, { "id": "high" }], "limit": { "context": 1000 } }),
            json!({ "id": "b", "providerID": "p", "enabled": false }),
            json!({ "id": "c", "providerID": "q" }),
        ];
        let catalog = open_code_catalog(
            &models,
            &[json!({ "id": "p", "name": "Provider" })],
            Some(&json!({ "id": "c", "providerID": "q" })),
        );
        assert_eq!(
            catalog
                .iter()
                .map(|spec| spec.id.as_str())
                .collect::<Vec<_>>(),
            ["q/c", "p/a"]
        );
        assert_eq!(
            catalog[1]
                .efforts
                .iter()
                .map(|effort| effort.id)
                .collect::<Vec<_>>(),
            [AgentEffort::High]
        );
        assert_eq!(catalog[1].provider.as_deref(), Some("Provider"));
        assert_eq!(
            model_ref("openrouter/z-ai/glm", Some("high")).unwrap(),
            json!({ "providerID": "openrouter", "id": "z-ai/glm", "variant": "high" })
        );
        assert!(model_ref("plain", None).is_none());
    }
}
