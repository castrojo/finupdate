use serde::Deserialize;
use std::collections::HashMap;
use std::process::Command;
use std::fs;
use crate::update_worker::is_flatpak;

/// A package change representation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PackageDiff {
    pub name: String,
    pub old_version: String,
    pub new_version: String,
}

/// The overall SBOM diff result.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SbomDiffResult {
    pub upgraded: Vec<PackageDiff>,
    pub removed: Vec<String>,
    pub added: Vec<PackageDiff>,
}

#[derive(Deserialize)]
struct SpdxDocument {
    packages: Option<Vec<SpdxPackage>>,
}

#[derive(Deserialize, Clone)]
struct SpdxPackage {
    name: String,
    #[serde(rename = "versionInfo")]
    version_info: Option<String>,
}

fn run_oras_command(args: &[&str]) -> Option<std::process::Output> {
    let candidates = if is_flatpak() {
        vec![
            ("flatpak-spawn", vec!["--host", "oras"]),
            ("flatpak-spawn", vec!["--host", "/home/linuxbrew/.linuxbrew/bin/oras"]),
            ("flatpak-spawn", vec!["--host", "/home/james/.linuxbrew/bin/oras"]),
            ("flatpak-spawn", vec!["--host", "/usr/local/bin/oras"]),
            ("flatpak-spawn", vec!["--host", "/usr/bin/oras"]),
        ]
    } else {
        vec![
            ("oras", vec![]),
            ("/home/linuxbrew/.linuxbrew/bin/oras", vec![]),
            ("/usr/local/bin/oras", vec![]),
            ("/usr/bin/oras", vec![]),
        ]
    };

    for (cmd_bin, cmd_args) in candidates {
        let mut c = Command::new(cmd_bin);
        c.args(&cmd_args);
        c.args(args);
        if let Ok(output) = c.output() {
            if output.status.success() {
                return Some(output);
            }
        }
    }
    None
}

fn get_sbom_paths(digest: &str) -> (String, String) {
    let digest_safe = digest.replace(':', "_");
    if is_flatpak() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/james".to_string());
        let read_path = format!("{}/.cache/finupdate/sbom/{}", home, digest_safe);
        
        let user = std::env::var("USER").unwrap_or_else(|_| "james".to_string());
        let write_path = format!(
            "/var/home/{}/.var/app/org.projectbluefin.Finupdate.Devel/cache/finupdate/sbom/{}",
            user, digest_safe
        );
        (write_path, read_path)
    } else {
        let path = format!("/tmp/finupdate/sbom/{}", digest_safe);
        (path.clone(), path)
    }
}

fn get_spdx_referrer_digest(image_ref: &str) -> Option<String> {
    let output = run_oras_command(&["discover", "--format", "json", "--depth", "1", image_ref])?;
    let val: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let referrers = val.get("referrers")?.as_array()?;
    
    for ref_val in referrers {
        let art_type = ref_val.get("artifactType").and_then(|v| v.as_str()).unwrap_or("");
        if art_type == "application/vnd.spdx+json" {
            return ref_val.get("digest").and_then(|v| v.as_str()).map(|s| s.to_string());
        }
    }
    None
}

fn pull_and_parse_sbom(image_ref: &str, digest: &str) -> Option<HashMap<String, String>> {
    let (temp_dir_host, temp_dir_sandbox) = get_sbom_paths(digest);
    let _ = fs::create_dir_all(&temp_dir_sandbox);

    let repo_part = image_ref.split(':').next()?;
    let pull_ref = format!("{}@{}", repo_part, digest);

    let _output = run_oras_command(&["pull", "--output", &temp_dir_host, &pull_ref])?;

    let entries = fs::read_dir(&temp_dir_sandbox).ok()?;
    let mut spdx_file_path = None;
    for entry in entries {
        if let Ok(entry) = entry {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                spdx_file_path = Some(path);
                break;
            }
        }
    }

    let file_path = spdx_file_path?;
    let file_content = fs::read_to_string(file_path).ok()?;
    let doc: SpdxDocument = serde_json::from_str(&file_content).ok()?;

    let mut packages_map = HashMap::new();
    if let Some(packages) = doc.packages {
        for pkg in packages {
            let version = pkg.version_info.unwrap_or_else(|| "unknown".to_string());
            packages_map.insert(pkg.name, version);
        }
    }
    
    let _ = fs::remove_dir_all(&temp_dir_sandbox);

    Some(packages_map)
}

pub fn diff_packages(
    booted_map: &HashMap<String, String>,
    target_map: &HashMap<String, String>,
) -> SbomDiffResult {
    let mut upgraded = Vec::new();
    let mut removed = Vec::new();
    let mut added = Vec::new();

    for (name, booted_ver) in booted_map {
        if let Some(target_ver) = target_map.get(name) {
            if booted_ver != target_ver {
                upgraded.push(PackageDiff {
                    name: name.clone(),
                    old_version: booted_ver.clone(),
                    new_version: target_ver.clone(),
                });
            }
        } else {
            removed.push(name.clone());
        }
    }

    for (name, target_ver) in target_map {
        if !booted_map.contains_key(name) {
            added.push(PackageDiff {
                name: name.clone(),
                old_version: "".to_string(),
                new_version: target_ver.clone(),
            });
        }
    }

    upgraded.sort_by(|a, b| a.name.cmp(&b.name));
    removed.sort();
    added.sort_by(|a, b| a.name.cmp(&b.name));

    SbomDiffResult { upgraded, removed, added }
}

pub fn fetch_and_diff_sboms(
    booted_ref: String,
    target_ref: String,
) -> Option<SbomDiffResult> {
    println!("[debug] sbom_diff: starting diff between {} and {}", booted_ref, target_ref);
    
    let booted_digest = get_spdx_referrer_digest(&booted_ref)?;
    println!("[debug] sbom_diff: booted spdx digest = {}", booted_digest);
    
    let target_digest = get_spdx_referrer_digest(&target_ref)?;
    println!("[debug] sbom_diff: target spdx digest = {}", target_digest);
    
    let booted_map = pull_and_parse_sbom(&booted_ref, &booted_digest)?;
    println!("[debug] sbom_diff: parsed {} booted packages", booted_map.len());
    
    let target_map = pull_and_parse_sbom(&target_ref, &target_digest)?;
    println!("[debug] sbom_diff: parsed {} target packages", target_map.len());
    
    let result = diff_packages(&booted_map, &target_map);
    println!(
        "[debug] sbom_diff: diff complete. upgraded={}, removed={}, added={}",
        result.upgraded.len(),
        result.removed.len(),
        result.added.len()
    );
    Some(result)
}
