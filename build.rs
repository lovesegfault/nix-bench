fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto/nix_bench.proto");
    tonic_build::compile_protos("proto/nix_bench.proto")?;
    Ok(())
}
