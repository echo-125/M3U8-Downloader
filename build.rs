// 把 assets/icon.ico 嵌入 exe 资源，窗口和任务栏才能显示自定义图标。
fn main() {
    #[cfg(windows)]
    {
        embed_resource::compile("app.rc", embed_resource::NONE)
            .manifest_required()
            .expect("嵌入 exe 图标资源失败");
    }
}
