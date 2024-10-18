fn main() -> std::io::Result<()> {
    let mut entries: Vec<String> = std::fs::read_dir("src/protos")?
        .map(|res| res.map(|e| e.path()))
        .collect::<Result<Vec<_>, std::io::Error>>()?
        .iter()
        .map(|p| p.to_str().unwrap().to_string())
        .collect();
    entries.sort();
    prost_build::compile_protos(&entries, &["protos/"])?;
    Ok(())
}
