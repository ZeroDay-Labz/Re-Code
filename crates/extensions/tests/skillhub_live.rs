//! Live checks against the real GitHub, skills.sh and Hermes services. They spend anonymous GitHub
//! API requests (60 an hour) and the Hermes one downloads about 42 MB, so they're ignored by default:
//! `cargo test -p recode-extensions --test skillhub_live -- --ignored`.

use recode_core::extensions::InstallOutcome;
use recode_extensions::fetch::{Bases, Fetcher};
use recode_extensions::skillhub::Hub;

fn hub(dir: &std::path::Path) -> Hub {
    Hub::new(Fetcher::new(Bases::live()).unwrap(), dir.join("cache"), dir.join("skills"))
}

#[tokio::test]
#[ignore = "talks to GitHub"]
async fn live_official_hermes_skill_stages_and_installs() {
    let dir = tempfile::tempdir().unwrap();
    let hub = hub(dir.path());
    let outcome = hub.stage("official/security/godmode").await;
    let InstallOutcome::Preview(preview) = outcome else { panic!("{outcome:?}") };
    println!("{} [{}] pinned @ {}", preview.name, preview.source.label, &preview.source.sha[..8]);
    println!("{} · {} files · ~{} tokens", preview.declares.summary(), preview.files.len(), preview.estimated_tokens);
    println!(
        "scan: {:?}",
        preview.scan.findings().iter().map(|f| format!("{}:{} {}", f.file, f.line, f.reason)).collect::<Vec<_>>()
    );
    assert_eq!(preview.name, "godmode");
    assert_eq!(preview.source.sha.len(), 40);
    assert!(preview.files.contains(&"SKILL.md".to_owned()));
    let committed = hub.commit(&preview.staging, false, false).unwrap();
    assert!(dir.path().join("skills/godmode/SKILL.md").is_file(), "{committed:?}");
    assert_eq!(
        hub.update("godmode").await,
        InstallOutcome::UpToDate { name: "godmode".into(), sha: preview.source.sha }
    );
}

#[tokio::test]
#[ignore = "talks to GitHub"]
async fn live_anthropics_pdf_skill_by_path_and_by_skills_sh_link() {
    let dir = tempfile::tempdir().unwrap();
    let hub = hub(dir.path());
    let InstallOutcome::Preview(preview) = hub.stage("anthropics/skills/skills/pdf").await else { panic!() };
    println!(
        "{} [{}] @ {} · {} files · {:?}",
        preview.name,
        preview.source.label,
        &preview.source.sha[..8],
        preview.files.len(),
        preview.declares
    );
    assert_eq!(preview.name, "pdf");
    assert!(preview.files.iter().any(|f| f.starts_with("scripts/")));
    hub.discard(&preview.staging).unwrap();
    let InstallOutcome::Preview(preview) = hub.stage("https://skills.sh/anthropics/skills/pdf").await else { panic!() };
    println!("skills.sh → {} note: {:?}", preview.source.label, preview.note);
    assert_eq!(preview.name, "pdf");
    hub.discard(&preview.staging).unwrap();
    // A whole repository offers its skills.
    let outcome = hub.stage("https://github.com/anthropics/skills").await;
    let InstallOutcome::Choose { candidates, .. } = outcome else { panic!("{outcome:?}") };
    println!(
        "{} skills offered: {:?}",
        candidates.len(),
        candidates.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
    );
    assert!(candidates.iter().any(|c| c.name == "pdf"));
}

#[tokio::test]
#[ignore = "downloads the 42 MB Hermes index"]
async fn live_hermes_index_search_and_lookup() {
    let dir = tempfile::tempdir().unwrap();
    let hub = hub(dir.path());
    let started = std::time::Instant::now();
    let search = hub.search("godmode", 5).await.unwrap();
    println!("search took {:?}: {:?}", started.elapsed(), search.hits.iter().map(|h| &h.tap).collect::<Vec<_>>());
    assert!(search.hits.iter().any(|hit| hit.name == "godmode"));
    let outcome = hub.stage("godmode").await;
    match outcome {
        InstallOutcome::Preview(preview) => {
            println!("godmode resolved to {} / {}", preview.source.repo, preview.source.path)
        }
        InstallOutcome::Choose { candidates, .. } => {
            println!("godmode is ambiguous: {:?}", candidates.iter().map(|c| &c.tap).collect::<Vec<_>>())
        }
        other => panic!("{other:?}"),
    }
}
