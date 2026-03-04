fn main() -> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path().unwrap());
    }
    prost_build::compile_protos(
        &[
            "../../proto/ahand/v1/envelope.proto",
            "../../proto/ahand/v1/browser.proto",
        ],
        &["../../proto"],
    )?;
    Ok(())
}
