fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../proto/nix_bench.proto");
    println!("cargo:rerun-if-changed={proto_path}");
    tonic_prost_build::compile_protos(proto_path)?;
    Ok(())
}
