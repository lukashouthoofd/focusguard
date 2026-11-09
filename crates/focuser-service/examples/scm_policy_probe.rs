//! CI helper for the Windows SCM recovery smoke test. This talks to the
//! service only through the same SCM-PID-checked named-pipe client as the UI.

use std::{env, thread, time::Duration};

use anyhow::bail;
use chrono::{NaiveTime, Weekday};
use focuser_app::{Command, CommandResult};
use focuser_client::execute;
use focuser_common::types::{AppMatchType, TimeSlot, WebsiteMatchType};

const LIST_NAME: &str = "FocusGuard SCM recovery CI";
const TEST_DOMAIN: &str = "focuser-scm-recovery.invalid";
const DOH_TEST_DOMAIN: &str = "example.com";
const TEST_APP_IMAGE: &str = "scm_policy_probe.exe";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if env::var("GITHUB_ACTIONS").ok().as_deref() != Some("true")
        || env::var("RUNNER_OS").ok().as_deref() != Some("Windows")
        || env::var("RUNNER_ENVIRONMENT").ok().as_deref() != Some("github-hosted")
        || env::var("FOCUSGUARD_SCM_RUNTIME_TEST").ok().as_deref() != Some("1")
    {
        bail!("this probe is restricted to the GitHub-hosted Windows SCM test job");
    }

    match env::args().nth(1).as_deref() {
        Some("seed") => seed().await,
        Some("verify") => verify().await,
        Some("process-target") => process_target(),
        _ => bail!("usage: scm_policy_probe.exe <seed|verify|process-target>"),
    }
}

async fn seed() -> anyhow::Result<()> {
    let result = execute(Command::CreateBlockList {
        name: LIST_NAME.into(),
    })
    .await?;
    let CommandResult::BlockList(list) = result else {
        bail!("service returned no block list after creation");
    };

    execute(Command::ToggleBlockList {
        id: list.id,
        enabled: true,
    })
    .await?;
    execute(Command::AddWebsiteRule {
        list_id: list.id,
        rule: WebsiteMatchType::Domain(TEST_DOMAIN.into()),
    })
    .await?;
    execute(Command::AddWebsiteRule {
        list_id: list.id,
        rule: WebsiteMatchType::Domain(DOH_TEST_DOMAIN.into()),
    })
    .await?;
    execute(Command::AddAppRule {
        list_id: list.id,
        rule: AppMatchType::ExecutableName(TEST_APP_IMAGE.into()),
    })
    .await?;
    execute(Command::UpdateSchedule {
        list_id: list.id,
        slots: [
            Weekday::Mon,
            Weekday::Tue,
            Weekday::Wed,
            Weekday::Thu,
            Weekday::Fri,
            Weekday::Sat,
            Weekday::Sun,
        ]
        .into_iter()
        .map(|day| TimeSlot::new(day, NaiveTime::MIN, NaiveTime::MIN))
        .collect(),
        always_active: false,
    })
    .await?;
    println!("Authenticated service IPC created exact-domain rules and a full-week schedule.");
    Ok(())
}

async fn verify() -> anyhow::Result<()> {
    let result = execute(Command::ListBlockLists).await?;
    let CommandResult::BlockLists(lists) = result else {
        bail!("service returned no block-list collection");
    };

    let found = lists.iter().find(|list| list.name == LIST_NAME);
    let Some(list) = found else {
        bail!("service did not return the persisted CI block list");
    };
    for domain in [TEST_DOMAIN, DOH_TEST_DOMAIN] {
        if !list
            .websites
            .iter()
            .any(|rule| rule.enabled && rule.match_type == WebsiteMatchType::Domain(domain.into()))
        {
            bail!("service did not return the persisted exact-domain rule for {domain}");
        }
    }
    if !list.applications.iter().any(|rule| {
        rule.enabled
            && matches!(
                &rule.match_type,
                AppMatchType::ExecutableName(name) if name == TEST_APP_IMAGE
            )
    }) {
        bail!("service did not persist the executable-name app rule");
    }
    let Some(schedule) = &list.schedule else {
        bail!("service did not persist the full-week CI schedule");
    };
    if schedule.time_slots.len() != 7 || !list.is_effectively_active() {
        bail!("service did not keep the full-week CI schedule active");
    }

    check_extension_presence().await?;

    println!(
        "Authenticated service IPC confirms both domain rules and the schedule are persisted and active."
    );
    Ok(())
}

/// Long-lived, windowless CI target used to verify the service's real
/// executable-name enforcement after SCM recovery.
fn process_target() -> anyhow::Result<()> {
    thread::sleep(Duration::from_secs(300));
    Ok(())
}

async fn check_extension_presence() -> anyhow::Result<()> {
    let result = execute(Command::GetBlockingHealth).await?;
    let CommandResult::BlockingHealth(health) = result else {
        bail!("service returned no blocking health after Native Messaging event");
    };
    if !health.extension_connected {
        bail!("service did not scope extension presence to the authenticated session");
    }
    println!(
        "Service health reports Native Messaging extension presence for this Windows session."
    );
    Ok(())
}
