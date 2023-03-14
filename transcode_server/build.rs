fn main() {
    let proto_path = "proto/transcode.proto";
    let out_dir = std::env::var("OUT_DIR").unwrap();

    tonic_build::configure()
        .build_server(false)
        .out_dir(&out_dir)
        .compile(&[proto_path], &[&"proto"])
        .unwrap();
}
