//! Live checks of the plugin hub against the real Hermes catalog, GitHub and Anthropic's marketplace.
//! They spend anonymous GitHub API requests, so they're ignored by default:
//! `cargo test -p recode-extensions --test plugins_live -- --ignored`.

use recode_core::extensions::{PluginOutcome, PluginPart};
use recode_extensions::fetch::{Bases, Fetcher};
use recode_extensions::plugins::PluginHub;

fn hub(dir: &std::path::Path) -> PluginHub {
    PluginHub::new(Fetcher::new(Bases::live()).unwrap(), dir.join("cache"), dir.join("plugins"))
}

#[tokio::test]
#[ignore = "talks to GitHub and the Hermes catalog"]
async fn live_hermes_catalog_plugin_shows_hermes_summary() {
    let dir = tempfile::tempdir().unwrap();
    let hub = hub(dir.path());
    let outcome = hub.stage("better-session-appearance", &[]).await;
    let PluginOutcome::Preview(preview) = outcome else { panic!("{outcome:?}") };
    println!("{}", preview.plugin.header());
    println!("{} · {} files", preview.plugin.capability_sentence(), preview.files);
    for part in &preview.plugin.parts {
        println!("  {}", part.describe());
    }
    assert_eq!(preview.plugin.label, "community");
    assert_eq!(preview.plugin.source.sha.len(), 40);
    assert!(preview.plugin.parts.iter().all(|part| !part.runs_here()), "a desktop plugin runs nothing here");
    let committed = hub.commit(&preview.staging, true).unwrap();
    println!("committed: {committed:?}");
    let search = hub.search("session", &[], 5).await.unwrap();
    println!("search: {:?}", search.iter().map(|l| &l.name).collect::<Vec<_>>());
    assert!(!search.is_empty());
}

#[tokio::test]
#[ignore = "talks to GitHub"]
async fn live_anthropic_marketplace_is_followed_and_context7_installs() {
    let dir = tempfile::tempdir().unwrap();
    let hub = hub(dir.path());
    let market = hub.add_marketplace("anthropics/claude-plugins-official").await.unwrap();
    println!("{} @ {} · {} plugins", market.name, &market.sha[..8], market.plugin_count);
    assert!(market.plugin_count > 100);
    let markets = vec![market];
    let listings = hub.browse("claude-plugins-official", &markets).await.unwrap();
    let refused = listings.iter().filter(|l| l.unsupported.is_some()).count();
    println!("{} listings, {refused} refused", listings.len());
    let PluginOutcome::Preview(preview) = hub.stage("context7@claude-plugins-official", &markets).await else {
        panic!()
    };
    println!("{}", preview.plugin.header());
    println!("{}", preview.plugin.capability_sentence());
    for part in &preview.plugin.parts {
        println!("  {}", part.describe());
    }
    assert!(preview.plugin.parts.iter().any(|part| matches!(part, PluginPart::Mcp { name, .. } if name == "context7")));
    hub.discard(&preview.staging).unwrap();
    // A plugin with hooks: the conversion is written and the hooks aren't loaded.
    let PluginOutcome::Preview(preview) = hub.stage("explanatory-output-style@claude-plugins-official", &markets).await
    else {
        panic!()
    };
    println!("{} · {:?}", preview.plugin.header(), preview.note);
    assert!(preview.plugin.parts.iter().any(|part| matches!(part, PluginPart::Hook { converted: true, .. })));
    assert!(std::path::Path::new(&preview.staging).join("hooks.converted.toml").is_file());
    hub.discard(&preview.staging).unwrap();
}
