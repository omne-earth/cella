// The service stubs, generated from the one vocabulary. prost in
// cella-libs generates the messages for the file wire; this build
// generates the same messages plus the Engine service for the
// stream. Both read the same bytes: the .proto is the contract.
fn main() {
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&["../../proto/cella.proto"], &["../../proto"])
        .expect("compiling proto/cella.proto");
    println!("cargo:rerun-if-changed=../../proto/cella.proto");
}
