//! End-to-end tests of tool dispatch against a real temporary workspace.

use recode_tools::{Workspace, run};
use serde_json::json;

fn workspace() -> (tempfile::TempDir, Workspace) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("main.rs"), "fn main() {\n    println!(\"hi\");\n}\n").unwrap();
    let ws = Workspace::new(dir.path()).unwrap();
    (dir, ws)
}

#[test]
fn read_list_grep_write_edit_delete_round_trip() {
    let (_dir, ws) = workspace();

    let read = run("read", &json!({"path": "main.rs"}), &ws);
    assert!(read.success && read.content.contains("1  fn main()"));

    let list = run("list", &json!({"path": "."}), &ws);
    assert!(list.content.contains("main.rs"));

    let grep = run("grep", &json!({"pattern": "println"}), &ws);
    assert!(grep.content.contains("main.rs:2"), "{}", grep.content);

    let write = run("write", &json!({"path": "new.txt", "content": "fresh\n"}), &ws);
    assert!(write.success && write.diff.as_ref().unwrap().contains("+fresh"));

    let edit = run("edit", &json!({"path": "main.rs", "old": "hi", "new": "bye"}), &ws);
    assert!(edit.success && edit.diff.as_ref().unwrap().contains("bye"));
    assert!(run("read", &json!({"path": "main.rs"}), &ws).content.contains("bye"));

    let delete = run("delete", &json!({"path": "new.txt"}), &ws);
    assert!(delete.success);
    assert!(!run("read", &json!({"path": "new.txt"}), &ws).success);
}

#[test]
fn errors_come_back_for_the_model_to_recover() {
    let (_dir, ws) = workspace();
    assert!(!run("read", &json!({"path": "missing"}), &ws).success);
    assert!(!run("edit", &json!({"path": "main.rs", "old": "zzz", "new": "y"}), &ws).success);
    assert!(run("read", &json!({}), &ws).content.contains("needs a 'path'"));
    assert!(run("nonsense", &json!({}), &ws).content.contains("Unknown tool"));
}

#[test]
fn shell_and_git_run_in_the_workspace() {
    let (_dir, ws) = workspace();
    let shell = run("shell", &json!({"command": "echo hello && ls"}), &ws);
    assert!(shell.success && shell.content.contains("hello") && shell.content.contains("main.rs"));
    // git with a disallowed subcommand is refused.
    assert!(run("git", &json!({"args": "push"}), &ws).content.contains("isn't allowed"));
}
