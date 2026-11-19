//! Library maintenance through the regular `.bazza` export/import path.
//! Close Bazza first; open only a copy of a library you must not change.
//!
//!   merge  <source-library> <target-library> <bundle.bazza>  copy all meetings; existing IDs are skipped
//!   delete <library> <meeting-id>                            same steps as the app's Delete action
use bazza_store::{database_path, Store};
use std::{error::Error, path::Path};

fn open(root: &str) -> Result<Store, Box<dyn Error>> {
    Ok(Store::open_legacy(&database_path(Path::new(root)))?)
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["merge", source, target, bundle] => {
            let source = open(source)?;
            let ids: Vec<String> = source.list_meetings()?.into_iter().map(|m| m.id).collect();
            bazza_share::export_bundle(&source, &ids, Path::new(bundle), true)?;
            drop(source);
            let report = bazza_share::import_bundle(&open(target)?, Path::new(bundle))?;
            println!(
                "exported {}, imported {}, skipped existing {}",
                ids.len(),
                report.imported,
                report.skipped_existing
            );
            for error in &report.errors {
                eprintln!("error: {error}");
            }
            if !report.errors.is_empty() {
                std::process::exit(1);
            }
        }
        ["delete", library, id] => {
            if id.len() != 36 || !id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
                return Err("expected a meeting UUID".into());
            }
            open(library)?.delete_meeting(id)?;
            let dir = Path::new(library).join("recordings").join(id);
            if dir.exists() {
                std::fs::remove_dir_all(dir)?;
            }
            println!("deleted {id}");
        }
        _ => {
            return Err(
                "usage: merge <source> <target> <bundle> | delete <library> <meeting-id>".into(),
            )
        }
    }
    Ok(())
}
