use br8n::model::{Chunk, Document, SourceType};
use br8n::store::Store;

#[test]
fn probe_insert_scaling() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), 4).unwrap();
    let text = "retrieval pipeline vector ".repeat(80);
    for round in 0..6 {
        let doc = Document::new(
            SourceType::Markdown,
            &format!("file:///d{round}.md"),
            "D",
            &text,
        );
        store.upsert_document(&doc).unwrap();
        let chunks: Vec<Chunk> = (0..200)
            .map(|i| Chunk {
                id: format!("{}:{i}", doc.id),
                doc_id: doc.id.clone(),
                ord: i,
                text: text.clone(),
                embed_text: text.clone(),
                heading_path: String::new(),
                page_no: None,
            })
            .collect();
        let vecs = vec![vec![0.5f32; 4]; 200];
        let t = std::time::Instant::now();
        store.insert_chunks(&doc.id, &chunks, &vecs).unwrap();
        let d = t.elapsed();
        println!(
            "SCALE table≈{:5} chunks | +200 took {:7.0} ms | {:6.1} chunks/s",
            round * 200,
            d.as_millis(),
            200.0 / d.as_secs_f64()
        );
    }
}
