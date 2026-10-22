use claudesktop_core::agents;

#[test]
#[ignore = "要起子进程，实机手动跑：cargo test -p claudesktop-core --test discover_agents -- --ignored --nocapture"]
fn discovers_what_is_installed_here() {
    let found = agents::discover();
    for d in &found {
        println!("{} {} → {} ({:?})", d.preset.name, d.path.display(), d.summary(), d.state);
    }
    for p in agents::PRESETS {
        if !found.iter().any(|d| d.preset.id == p.id) {
            println!("{} 未安装（装法：{}）", p.name, p.install_hint);
        }
    }
    println!("PATH 之外还查了：{:?}", agents::common_install_dirs());
}
