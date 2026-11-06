fn main() {
    println!("cargo:rerun-if-changed=proto");
    if std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows") {
        println!("cargo:rustc-link-lib=advapi32");
    }
    protobuf_codegen::Codegen::new()
        .pure()
        .includes(["proto"])
        .inputs([
            "proto/common/envelope.proto",
            "proto/common/commands/base_commands.proto",
            "proto/common/commands/project_commands.proto",
            "proto/common/commands/editor_commands.proto",
            "proto/common/types/base_types.proto",
            "proto/common/types/enums.proto",
            "proto/common/types/embedded_files.proto",
            "proto/common/types/project_settings.proto",
            "proto/board/board.proto",
            "proto/board/board_types.proto",
            "proto/board/board_commands.proto",
        ])
        .cargo_out_dir("generated")
        .run_from_script();
}
