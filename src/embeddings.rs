//! Deterministic local embeddings. This compact signed feature-hashing model ships in the binary,
//! performs no network or model download, and has a versioned identity for durable vectors.
pub const MODEL: &str = "harness-local-hash-v1";
pub const DIMENSIONS: usize = 256;

fn hash(bytes: &[u8]) -> u64 {
    let mut value = 0xcbf29ce484222325u64;
    for byte in bytes {
        value ^= *byte as u64;
        value = value.wrapping_mul(0x100000001b3);
    }
    value
}

fn add(vector: &mut [f32], feature: &str, weight: f32) {
    let h = hash(feature.as_bytes());
    let index = (h as usize) % vector.len();
    let sign = if h & (1 << 63) == 0 { 1.0 } else { -1.0 };
    vector[index] += sign * weight;
}

pub fn embed(text: &str) -> Vec<f32> {
    let normalized = text.to_lowercase();
    let tokens = normalized
        .split(|c: char| !c.is_alphanumeric())
        .filter(|v| !v.is_empty())
        .collect::<Vec<_>>();
    let mut vector = vec![0.0; DIMENSIONS];
    for token in &tokens {
        add(&mut vector, &format!("w:{token}"), 1.0);
        let chars = token.chars().collect::<Vec<_>>();
        for n in 2..=3 {
            if chars.len() < n {
                continue;
            }
            for gram in chars.windows(n) {
                add(
                    &mut vector,
                    &format!("g{}:{}", n, gram.iter().collect::<String>()),
                    0.35,
                );
            }
        }
    }
    for pair in tokens.windows(2) {
        add(&mut vector, &format!("b:{} {}", pair[0], pair[1]), 0.5);
    }
    let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in &mut vector {
            *value /= norm;
        }
    }
    vector
}

pub fn encode(vector: &[f32]) -> Vec<u8> {
    vector
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

pub fn decode(bytes: &[u8], dimensions: usize) -> Option<Vec<f32>> {
    if dimensions != DIMENSIONS || bytes.len() != dimensions * 4 {
        return None;
    }
    Some(
        bytes
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes(chunk.try_into().unwrap()))
            .collect(),
    )
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    a.iter()
        .zip(b)
        .map(|(x, y)| x * y)
        .sum::<f32>()
        .clamp(-1.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vectors_are_deterministic_normalized_and_round_trip() {
        let first = embed("Rust memory engine");
        let second = embed("Rust memory engine");
        assert_eq!(first, second);
        assert!((cosine(&first, &first) - 1.0).abs() < 0.0001);
        assert_eq!(decode(&encode(&first), DIMENSIONS).unwrap(), first);
    }
    #[test]
    fn character_features_recall_related_spelling() {
        let related = cosine(&embed("Rust project"), &embed("rustacean projects"));
        let unrelated = cosine(&embed("Rust project"), &embed("banana orchestra"));
        assert!(
            related > unrelated && related > 0.05,
            "related={related} unrelated={unrelated}"
        );
    }
}
