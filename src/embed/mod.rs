pub mod ollama;
pub use ollama::OllamaEmbedder;
pub mod openai;
pub use openai::OpenAiEmbedder;

use anyhow::Result;

pub trait Embedder: Send + Sync {
    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    fn embed_query(&self, text: &str) -> Result<Vec<f32>>;
    /// Issues a throwaway embed to pin the model resident in Ollama. Called by
    /// the `SessionStart` hook so the first real query doesn't pay the cold-load
    /// cost (Spike 2: ~2000ms cold vs ~24ms warm).
    fn warm(&self) -> Result<()>;
    /// Stamped into the index. A mismatch is a hard error, never a silent fallback.
    fn model_id(&self) -> String;
    fn dimensions(&self) -> usize;
}

impl<E: Embedder + ?Sized> Embedder for std::sync::Arc<E> {
    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        (**self).embed_documents(texts)
    }
    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        (**self).embed_query(text)
    }
    fn warm(&self) -> Result<()> {
        (**self).warm()
    }
    fn model_id(&self) -> String {
        (**self).model_id()
    }
    fn dimensions(&self) -> usize {
        (**self).dimensions()
    }
}

pub fn normalize(v: Vec<f32>) -> Vec<f32> {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm == 0.0 {
        return v;
    }
    v.into_iter().map(|x| x / norm).collect()
}

/// Matryoshka truncation: taking a prefix of the vector and renormalizing is valid
/// for models trained with Matryoshka loss (Qwen3-Embedding is).
pub fn fit_dimensions(v: Vec<f32>, dims: usize) -> Vec<f32> {
    let mut v = v;
    if v.len() > dims {
        v.truncate(dims);
    } else if v.len() < dims {
        v.resize(dims, 0.0);
    }
    normalize(v)
}

pub(crate) fn decode_vectors(
    raw: Vec<Vec<f32>>,
    expected: usize,
    dims: usize,
) -> Result<Vec<Vec<f32>>> {
    anyhow::ensure!(
        raw.len() == expected,
        "the endpoint returned {} embeddings for {} inputs",
        raw.len(),
        expected
    );
    raw.into_iter()
        .enumerate()
        .map(|(i, floats)| {
            anyhow::ensure!(
                !floats.is_empty(),
                "embedding {i} decoded to no floats; refusing to index a placeholder"
            );
            anyhow::ensure!(
                floats.len() >= dims,
                "model returned {} dimensions but {} are configured; lower \
                 `dimensions` in config or choose another model",
                floats.len(),
                dims
            );
            Ok(fit_dimensions(floats, dims))
        })
        .collect()
}

pub fn timed_out(e: &anyhow::Error) -> bool {
    e.chain()
        .filter_map(|c| c.downcast_ref::<reqwest::Error>())
        .any(reqwest::Error::is_timeout)
}

pub fn for_config(cfg: &crate::config::EmbedConfig) -> Result<Box<dyn Embedder>> {
    if let Some(e) = &cfg.remote_error {
        anyhow::bail!("{e}");
    }
    match &cfg.remote {
        Some(r) => Ok(Box::new(OpenAiEmbedder::new(cfg, r))),
        None => Ok(Box::new(OllamaEmbedder::new(cfg))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_count_mismatch_is_refused() {
        let err = decode_vectors(vec![vec![1.0, 0.0]], 2, 2)
            .unwrap_err()
            .to_string();
        assert!(err.contains("1 embeddings for 2 inputs"), "{err}");
    }

    #[test]
    fn an_empty_vector_is_refused() {
        let err = decode_vectors(vec![vec![]], 1, 2).unwrap_err().to_string();
        assert!(err.contains("refusing to index a placeholder"), "{err}");
    }

    #[test]
    fn a_narrow_vector_is_refused_naming_both_widths() {
        let err = decode_vectors(vec![vec![1.0, 0.0]], 1, 512)
            .unwrap_err()
            .to_string();
        assert!(err.contains("2 dimensions") && err.contains("512"), "{err}");
    }

    #[test]
    fn a_good_batch_is_fitted_to_the_configured_width() {
        let out = decode_vectors(vec![vec![3.0, 4.0, 9.0]], 1, 2).unwrap();
        assert_eq!(out[0].len(), 2);
        let norm: f32 = out[0].iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6, "must be renormalised: {norm}");
    }
}
