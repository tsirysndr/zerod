fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = std::path::PathBuf::from("src/generated");
    std::fs::create_dir_all(&out_dir)?;
    tonic_build::configure()
        .out_dir(&out_dir)
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
