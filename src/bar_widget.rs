//! The bar widget, offered once on the first start.
//!
//! The package installs the widget to `/usr/share`, but the Omarchy shell only
//! loads plugins from `~/.config/omarchy/plugins`, and a package has no
//! business writing in a home directory. So the app asks, and on a yes links
//! the widget there and puts it on the right of the bar.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use gtk::glib;

use crate::settings;

const ID: &str = "jankeesvw.meeting-recorder";
const SOURCE: &str = "/usr/share/omarchy-meeting-recorder/plugin";

fn target() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| glib::home_dir().join(".config"))
        .join("omarchy/plugins")
        .join(ID)
}

/// Offer on Omarchy for a package install until accepted successfully or declined.
pub fn should_offer() -> bool {
    // A failed attempt may have already created our symlink. Leave other
    // installations alone, but allow retrying the link we create in add().
    !settings::bar_widget_offered()
        && glib::find_program_in_path("omarchy").is_some()
        && PathBuf::from(SOURCE).join("manifest.json").is_file()
        && (std::fs::symlink_metadata(target()).is_err()
            || std::fs::read_link(target()).is_ok_and(|path| path == PathBuf::from(SOURCE)))
}

fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let message = [stderr.trim(), stdout.trim()]
            .into_iter()
            .find(|s| !s.is_empty())
            .unwrap_or("failed");
        Err(format!("{program}: {message}"))
    }
}

/// Links the widget into the shell's plugin folder and enables it. Blocking.
pub fn add() -> Result<(), String> {
    let target = target();
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    if std::fs::symlink_metadata(&target).is_err() {
        std::os::unix::fs::symlink(SOURCE, &target).map_err(|e| e.to_string())?;
    }
    enable(run, std::thread::sleep)
}

fn enable(
    mut run: impl FnMut(&str, &[&str]) -> Result<String, String>,
    mut sleep: impl FnMut(Duration),
) -> Result<(), String> {
    run("omarchy-shell", &["shell", "rescanPlugins"])?;
    // rescanPlugins only starts the shell's asynchronous manifest scan. Wait
    // for the live registry (not the on-disk catalog) before trying to enable.
    for attempt in 0..50 {
        let output = run("omarchy-shell", &["shell", "listPlugins"])?;
        let plugins: Vec<serde_json::Value> = serde_json::from_str(&output)
            .map_err(|e| format!("Could not read the shell's plugin list: {e}"))?;
        if plugins
            .iter()
            .any(|plugin| plugin["id"].as_str() == Some(ID))
        {
            run("omarchy", &["plugin", "enable", ID, "--section", "right"])?;
            return Ok(());
        }
        if attempt < 49 {
            sleep(Duration::from_millis(100));
        }
    }
    Err(format!(
        "The shell did not discover {ID} after rescanning. Try reopening the app to add it again."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    fn scenario(replies: Vec<Result<&str, &str>>) -> (Result<(), String>, Vec<String>, usize) {
        let mut replies: VecDeque<_> = replies.into();
        let mut calls = Vec::new();
        let mut sleeps = 0;
        let result = enable(
            |program, args| {
                calls.push(format!("{program} {}", args.join(" ")));
                replies
                    .pop_front()
                    .expect("unexpected command")
                    .map(str::to_owned)
                    .map_err(str::to_owned)
            },
            |duration| {
                assert_eq!(duration, Duration::from_millis(100));
                sleeps += 1;
            },
        );
        assert!(replies.is_empty());
        (result, calls, sleeps)
    }

    const FOUND: &str = r#"[{"id":"jankeesvw.meeting-recorder","enabled":false}]"#;

    #[test]
    fn waits_for_discovery_before_enabling() {
        let (result, calls, sleeps) = scenario(vec![
            Ok(""),
            Ok("[]"),
            Ok(r#"[{"id":"other.plugin"}]"#),
            Ok(FOUND),
            Ok("Enabled"),
        ]);
        assert_eq!(result, Ok(()));
        assert_eq!(sleeps, 2);
        assert_eq!(
            calls,
            [
                "omarchy-shell shell rescanPlugins",
                "omarchy-shell shell listPlugins",
                "omarchy-shell shell listPlugins",
                "omarchy-shell shell listPlugins",
                "omarchy plugin enable jankeesvw.meeting-recorder --section right",
            ]
        );
    }

    #[test]
    fn already_discovered_plugin_needs_no_sleep() {
        let (result, _, sleeps) = scenario(vec![Ok(""), Ok(FOUND), Ok("Enabled")]);
        assert_eq!(result, Ok(()));
        assert_eq!(sleeps, 0);
    }

    #[test]
    fn missing_plugin_times_out_without_enabling() {
        let mut replies = vec![Ok("")];
        replies.extend(vec![Ok("[]"); 50]);
        let (result, calls, sleeps) = scenario(replies);
        assert!(result.unwrap_err().contains("did not discover"));
        assert_eq!(sleeps, 49);
        assert!(calls.iter().all(|call| call.starts_with("omarchy-shell ")));
    }

    #[test]
    fn command_failures_are_preserved() {
        for replies in [
            vec![Err("shell unavailable")],
            vec![Ok(""), Err("shell unavailable")],
            vec![Ok(""), Ok(FOUND), Err("invalid placement")],
        ] {
            let expected = replies.last().unwrap().as_ref().unwrap_err().to_string();
            let (result, _, sleeps) = scenario(replies);
            assert_eq!(result, Err(expected));
            assert_eq!(sleeps, 0);
        }
    }

    #[test]
    fn malformed_plugin_list_is_reported_without_enabling() {
        for output in ["not JSON", "{}"] {
            let (result, calls, sleeps) = scenario(vec![Ok(""), Ok(output)]);
            assert!(result.unwrap_err().contains("Could not read"));
            assert_eq!(calls.len(), 2);
            assert_eq!(sleeps, 0);
        }
    }
}
