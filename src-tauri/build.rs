fn main() {
    // Compile encodemapping.proto with protox (pure Rust, no system protoc needed),
    // then hand the descriptors to prost/tonic codegen.
    let file_descriptors =
        protox::compile(["proto/encodemapping.proto"], ["proto"]).expect("proto compile failed");

    tonic_build::configure()
        .build_client(true)
        .build_server(false)
        .compile_fds(file_descriptors)
        .expect("tonic codegen failed");

    tauri_build::build();
}
