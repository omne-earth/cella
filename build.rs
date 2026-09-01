fn main() {
    // The service definition serves the engine side (tonic, outside
    // this crate); here only the message types generate.
    prost_build::compile_protos(&["proto/cella.proto"], &["proto/"])
        .expect("proto/cella.proto compiles");
    println!("cargo:rerun-if-changed=proto/cella.proto");
}
