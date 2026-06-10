fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = std::path::PathBuf::from("src/generated");
    std::fs::create_dir_all(&out_dir)?;
    tonic_build::configure()
        .out_dir(&out_dir)
        // proto3 `optional` was experimental in protoc 3.12–3.14 and stable in
        // 3.15+. Passing the flag explicitly lets the build work on older
        // protoc (e.g. the one in Debian 11's protobuf-compiler) and is a
        // no-op on modern protoc.
        .protoc_arg("--experimental_allow_proto3_optional")
        .file_descriptor_set_path(out_dir.join("zerod_descriptor.bin"))
        .compile_protos(
            &[
                "proto/zerod/v1alpha1/bluetooth.proto",
                "proto/zerod/v1alpha1/stream.proto",
                "proto/zerod/v1alpha1/systemd.proto",
                "proto/zerod/v1alpha1/config.proto",
                "proto/zerod/v1alpha1/system.proto",
            ],
            &["proto"],
        )?;
    Ok(())
}
