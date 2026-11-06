//! Usage: BAZZA_MODELS_DIR=<dir> cargo run -p bazza-models --example download_model -- <model-id>
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let id = std::env::args().nth(1).expect("model id");
    let dir = std::env::var("BAZZA_MODELS_DIR").map(std::path::PathBuf::from).unwrap_or_else(|_| bazza_store_dir());
    let cat = bazza_models::Catalog::builtin();
    let spec = cat.get(&id).expect("unknown model id");
    let last = std::sync::atomic::AtomicU64::new(0);
    let p = bazza_models::download(spec, &dir, |d, t| {
        let pct = d * 100 / t.max(1);
        if pct / 10 != last.swap(pct, std::sync::atomic::Ordering::Relaxed) / 10 { println!("{pct}%"); }
    }, || false).await.unwrap();
    println!("saved {p:?}");
}
fn bazza_store_dir() -> std::path::PathBuf {
    dirs::data_dir().unwrap().join("Bazza").join("models")
}
