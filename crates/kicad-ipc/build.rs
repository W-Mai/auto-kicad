fn main() {
    println!("cargo:rerun-if-changed=proto");
    protobuf_codegen::Codegen::new()
        .pure()
        .includes(["proto"])
        .inputs([
            "proto/common/envelope.proto",
            "proto/common/commands/base_commands.proto",
            "proto/common/types/base_types.proto",
            "proto/common/types/enums.proto",
        ])
        .cargo_out_dir("generated")
        .run_from_script();
}
