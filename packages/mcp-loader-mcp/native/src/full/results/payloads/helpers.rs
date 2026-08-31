use crate::full::*;

pub(crate) fn sorted_unique(values: &[String]) -> Vec<String> {
    let mut result = values.to_vec();
    result.sort();
    result.dedup();
    result
}

pub(crate) fn duplicate_strings(values: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut duplicates = HashSet::new();
    for value in values {
        if !seen.insert(value) {
            duplicates.insert(value.clone());
        }
    }
    let mut result: Vec<String> = duplicates.into_iter().collect();
    result.sort();
    result
}
