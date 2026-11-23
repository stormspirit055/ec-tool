fn main() {
    // 两个引擎由 `pnpm build-qc-engine` 构建。未构建时建空目录，开发和测试仍可编译；
    // 应用运行时找不到引擎会在界面上提示，对应步骤标记为失败。
    let _ = std::fs::create_dir_all("resources/qc-engine");
    let _ = std::fs::create_dir_all("resources/handpose-engine");
    tauri_build::build()
}
