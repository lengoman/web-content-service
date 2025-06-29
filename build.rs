fn main() {
    tonic_build::compile_protos("proto/web_content_service.proto")
        .unwrap();
} 