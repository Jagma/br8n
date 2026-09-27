use br8n::chunk::Chunker;
use br8n::model::{Document, SourceType};

#[test]
fn probe_real_sizes() {
    let para = "## Heading\n\nretrieval pipeline vector store embedding query rerank chunk graph fusion latency and more words here\n\n";
    for mb in [0.25f64, 0.5, 1.0, 2.0, 4.0] {
        let reps = (mb * 1_048_576.0 / para.len() as f64) as usize;
        let text = para.repeat(reps);
        let doc = Document::new(SourceType::Markdown, "file:///b.md", "B", &text);
        let t = std::time::Instant::now();
        let n = Chunker::new(512, 256).chunk(&doc).len();
        println!(
            "CHUNK {mb:5.2} MB -> {n:6} chunks in {:8.1} s",
            t.elapsed().as_secs_f64()
        );
    }
}
