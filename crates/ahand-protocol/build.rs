fn main() -> Result<(), Box<dyn std::error::Error>> {
    prost_build::compile_protos(
        &[
            "../../proto/ahand/v1/envelope.proto",
            "../../proto/ahand/v1/browser.proto",
        ],
        &["../../proto"],
    )?;
    Ok(())
}
