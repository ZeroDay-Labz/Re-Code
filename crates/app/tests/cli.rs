//! The command line against isolated configuration folders: lists, adds, installs from a local folder,
//! memory and trust, with their exit codes. Nothing here reaches the network.

use std::path::Path;
use std::process::Command;

struct Env {
    dir: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        for sub in ["config", "data", "cache", "state", "project", "home"] {
            std::fs::create_dir_all(dir.path().join(sub)).unwrap();
        }
        Self { dir }
    }

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let output = Command::new(env!("CARGO_BIN_EXE_re-code"))
            .args(args)
            .current_dir(self.dir.path().join("project"))
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env("XDG_DATA_HOME", self.dir.path().join("data"))
            .env("XDG_CACHE_HOME", self.dir.path().join("cache"))
            .env("XDG_STATE_HOME", self.dir.path().join("state"))
            .env("HOME", self.dir.path().join("home"))
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }
}

#[test]
fn mcp_servers_are_listed_added_switched_and_removed() {
    let env = Env::new();
    let (code, out, _) = env.run(&["mcp", "list", "--json"]);
    assert_eq!((code, out.trim()), (0, "[]"));
    let (code, out, err) = env.run(&["mcp", "add", "--name", "ide", "http://127.0.0.1:64522/stream"]);
    assert_eq!(code, 0, "{out}{err}");
    let (code, out, _) = env.run(&["mcp", "add", "--name", "local", "--no-sandbox", "npx -y thing-mcp"]);
    assert_eq!(code, 0, "{out}");
    let (_, out, _) = env.run(&["mcp", "list"]);
    assert!(out.contains("ide") && out.contains("http://127.0.0.1:64522/stream"), "{out}");
    assert!(out.contains("local") && out.contains("unsandboxed"), "{out}");
    let (code, _, _) = env.run(&["mcp", "disable", "ide"]);
    assert_eq!(code, 0);
    let (_, out, _) = env.run(&["mcp", "list", "--json"]);
    let servers: serde_json::Value = serde_json::from_str(&out).unwrap();
    let ide = servers.as_array().unwrap().iter().find(|s| s["name"] == "ide").unwrap();
    assert_eq!(ide["enabled"], false);
    let local = servers.as_array().unwrap().iter().find(|s| s["name"] == "local").unwrap();
    assert_eq!(local["sandbox"], "none");
    let (code, _, _) = env.run(&["mcp", "remove", "ide"]);
    assert_eq!(code, 0);
    // Removing it again is harmless (the store treats it as already gone).
    let (code, _, _) = env.run(&["mcp", "remove", "ide"]);
    assert!(code == 0 || code == 1);
    // A test of a server that doesn't exist is a failure with the reason, not a crash.
    let (code, _, err) = env.run(&["mcp", "test", "nothing"]);
    assert_eq!(code, 1);
    assert!(err.contains("no MCP server called nothing"), "{err}");
}

#[test]
fn a_skill_installs_from_a_folder_and_shows_the_preview() {
    let env = Env::new();
    let source = env.path().join("home/myskill");
    std::fs::create_dir_all(source.join("scripts")).unwrap();
    std::fs::write(
        source.join("SKILL.md"),
        "---\nname: myskill\ndescription: Does my thing. Use when asked.\n---\n\nFirst run `pip install x`.\n",
    )
    .unwrap();
    std::fs::write(source.join("scripts/run.py"), "print(1)").unwrap();
    // Without a terminal and without a flag it stops, installs nothing, and leaves staging clean.
    let (code, out, err) = env.run(&["skill", "install", source.to_str().unwrap()]);
    assert_eq!(code, 4, "{out}{err}");
    assert!(
        out.contains("myskill  [local]") && out.contains("scan: caution") && out.contains("ships a script"),
        "{out}"
    );
    assert!(err.contains("--yes or --keep-off"), "{err}");
    let (_, out, _) = env.run(&["skill", "list", "--json"]);
    assert_eq!(out.trim(), "[]");
    let (code, out, err) = env.run(&["skill", "install", "--json", "--keep-off", source.to_str().unwrap()]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("\"name\": \"myskill\"") && out.contains("Installed myskill (off)"), "{out}");
    let (_, out, _) = env.run(&["skill", "list"]);
    assert!(out.contains("off  myskill") && out.contains("local") && out.contains("caution"), "{out}");
    let (code, _, _) = env.run(&["skill", "enable", "myskill"]);
    assert_eq!(code, 0);
    let (_, out, _) = env.run(&["skill", "list", "--json"]);
    let skills: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(skills[0]["enabled"], true);
    assert_eq!(skills[0]["installed_from"]["label"], "local");
    let (code, _, err) = env.run(&["skill", "install", "--yes", "nothing/here/at/all/x"]);
    assert!(code == 2 || code == 1, "{err}");
    let (code, out, _) = env.run(&["skill", "remove", "myskill"]);
    assert_eq!(code, 0, "{out}");
}

#[test]
fn a_local_plugin_installs_and_a_marketplace_is_followed() {
    let env = Env::new();
    let market = env.path().join("home/market");
    std::fs::create_dir_all(market.join(".claude-plugin")).unwrap();
    std::fs::create_dir_all(market.join("plugins/hello/skills/hello")).unwrap();
    std::fs::write(
        market.join(".claude-plugin/marketplace.json"),
        r#"{"name": "mine", "plugins": [{"name": "hello", "description": "Says hi.", "source": "./plugins/hello"}]}"#,
    )
    .unwrap();
    std::fs::write(
        market.join("plugins/hello/.claude-plugin/plugin.json"),
        r#"{"name":"hello","description":"Says hi."}"#,
    )
    .unwrap_or_else(|_| {
        std::fs::create_dir_all(market.join("plugins/hello/.claude-plugin")).unwrap();
        std::fs::write(
            market.join("plugins/hello/.claude-plugin/plugin.json"),
            r#"{"name":"hello","description":"Says hi."}"#,
        )
        .unwrap();
    });
    std::fs::write(
        market.join("plugins/hello/skills/hello/SKILL.md"),
        "---\nname: hello\ndescription: Say hello.\n---\nSay it.",
    )
    .unwrap();
    let (code, out, err) = env.run(&["plugin", "marketplace", "add", market.to_str().unwrap()]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("Following mine (1 plugins)"), "{out}");
    let (_, out, _) = env.run(&["plugin", "marketplace", "browse", "mine"]);
    assert!(out.contains("hello@mine"), "{out}");
    let (code, out, err) = env.run(&["plugin", "install", "--yes", "hello@mine"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(
        out.contains("hello [mine]") && out.contains("brings 1 skill") && out.contains("Installed hello (on)"),
        "{out}"
    );
    let (_, out, _) = env.run(&["plugin", "list"]);
    assert!(out.contains("on   hello [mine]") && out.contains("marketplace  mine"), "{out}");
    // The plugin's skill is a skill now.
    let (_, out, _) = env.run(&["skill", "list"]);
    assert!(out.contains("hello") && out.contains("plugin"), "{out}");
    let (code, out, _) = env.run(&["plugin", "remove", "hello"]);
    assert_eq!(code, 0, "{out}");
    let (code, _, _) = env.run(&["plugin", "marketplace", "remove", "mine"]);
    assert_eq!(code, 0);
    let (_, out, _) = env.run(&["plugin", "marketplace", "list", "--json"]);
    assert_eq!(out.trim(), "[]");
}

#[test]
fn memory_and_trust_from_the_shell() {
    let env = Env::new();
    let (code, out, _) = env.run(&["memory", "list", "--json"]);
    assert_eq!((code, out.trim()), (0, "[]"));
    let (code, _, err) = env.run(&["memory", "show", "nothing"]);
    assert_eq!(code, 2, "{err}");
    // A project with configuration: trusting without a terminal needs --yes.
    let project = env.path().join("project");
    std::fs::create_dir_all(project.join(".re-code/skills/review")).unwrap();
    std::fs::write(project.join(".re-code/skills/review/SKILL.md"), "---\nname: review\ndescription: Review.\n---\nx")
        .unwrap();
    let (code, out, err) = env.run(&["trust"]);
    assert_eq!(code, 4, "{out}{err}");
    assert!(out.contains("skill  review"), "{out}");
    let (code, out, _) = env.run(&["trust", "--yes"]);
    assert_eq!(code, 0, "{out}");
    let (_, out, _) = env.run(&["skill", "list"]);
    assert!(out.contains("on   review"), "{out}");
    let (code, out, _) = env.run(&["untrust"]);
    assert_eq!(code, 0, "{out}");
    let (_, out, _) = env.run(&["skill", "list"]);
    assert!(out.contains("off  review"), "{out}");
}
