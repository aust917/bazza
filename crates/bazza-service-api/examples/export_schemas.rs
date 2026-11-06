fn main() -> Result<(), Box<dyn std::error::Error>> {
    let catalog = serde_json::json!({
        "contract_only": true,
        "implemented_capabilities": [],
        "tools": bazza_service_api::tool_schemas(),
    });
    println!("{}", serde_json::to_string_pretty(&catalog)?);
    Ok(())
}
