use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(canonicalize).collect()),
        Value::Object(object) => {
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort();
            let mut sorted = Map::new();
            for key in keys {
                sorted.insert(key.clone(), canonicalize(&object[key]));
            }
            Value::Object(sorted)
        }
        _ => value.clone(),
    }
}

pub fn canonical_json(value: &Value) -> String {
    serde_json::to_string(&canonicalize(value)).expect("JSON values must serialize")
}

pub fn stable_digest(value: &Value) -> String {
    hex::encode(Sha256::digest(canonical_json(value).as_bytes()))
}

pub fn binding_admission_entry_digest_v1(entry: &Value) -> String {
    let mut unsigned = entry.clone();
    let object = unsigned.as_object_mut().expect("binding admission entry must be an object");
    object.remove("binding_digest");
    let identity = object.remove("binding_identity").expect("binding admission entry must carry binding_identity");
    object.insert("launch_identity".to_string(), identity);
    stable_digest(&unsigned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_identity_golden_vectors_fix_native_contract() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../contracts/binding-identity-v1.vectors.json"
        )).unwrap();
        for vector in fixture["vectors"].as_array().unwrap() {
            assert_eq!(canonical_json(&vector["unsigned"]), vector["canonical_json"].as_str().unwrap());
            assert_eq!(stable_digest(&vector["unsigned"]), vector["sha256"].as_str().unwrap());
        }
    }
}
