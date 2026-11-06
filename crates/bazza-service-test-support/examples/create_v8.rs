fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let root = arguments
        .next()
        .ok_or("explicit new fixture directory required")?;
    if arguments.next().is_some() {
        return Err("expected exactly one directory".into());
    }
    let fixture = bazza_service_test_support::create_v8(std::path::Path::new(&root))?;
    println!("{}", fixture.root.display());
    Ok(())
}
