use std::collections::HashMap;
use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};

use serde::{self, Deserialize};

use anyhow::{anyhow, Result};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Mods {
    mods: Vec<Mod>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Mod {
    #[serde(rename = "ID")]
    id: u32,
    profile: ModProfile,
}

#[derive(Debug, Deserialize)]
struct ModProfile {
    name: String,
}

fn get_modio_dir() -> Result<PathBuf> {
    match std::env::consts::OS {
        "linux" => Ok(Path::new(&std::env::var("HOME")?).join(
            ".local/share/Steam/steamapps/compatdata/548430/pfx/drive_c/users/Public/mod.io/",
        )),
        "windows" => Ok(PathBuf::from("C:/Users/Public/mod.io")),
        _ => Err(anyhow!("unrecognized os")),
    }
}

fn main() -> Result<()> {
    let modio_path = if let Some(modio_path) = std::env::args().nth(1) {
        Ok(PathBuf::from(modio_path))
    } else {
        get_modio_dir()
    };
    let modio_path = modio_path
        .and_then(|path| {
            if path.exists() && path.is_dir() {
                Ok(path)
            } else {
                Err(anyhow!("{} is not a directory", path.display()))
            }
        })
        .map_err(|e| anyhow!("Could not find mod.io directory ({e}). Try manually specifying it as an argument if you haven't already."))?;
    let drg_modio_path = modio_path.join("2475");
    let state_path = drg_modio_path.join("metadata/state.json");
    let mods_path = drg_modio_path.join("mods");
    let state: Mods = serde_json::from_reader(BufReader::new(File::open(state_path)?))?;
    let mod_name_map = state
        .mods
        .into_iter()
        .map(|m| (m.id, m.profile.name))
        .collect::<HashMap<_, _>>();
    let mut asset_owners: HashMap<String, Vec<u32>> = HashMap::new();
    for m in fs::read_dir(mods_path)? {
        let m = m?;
        let mod_id = m.file_name().to_string_lossy().parse::<u32>()?;
        if let Some(path) = find_pak(m.path())? {
            match find_mod_assets(&path) {
                Ok(files) => {
                    for file in files {
                        asset_owners.entry(file).or_insert(vec![]).push(mod_id);
                    }
                }
                Err(e) => println!("error reading {}: {}", path.display(), e),
            }
        } else {
            println!("could not find .pak in {}", m.path().display());
        }
    }
    let mut sorted = asset_owners.into_iter().collect::<Vec<_>>();
    sorted.sort_by_key(|a| a.1.len());
    for asset in sorted {
        println!("{}", asset.0);
        println!("\tmodified by:");
        for mod_id in asset
            .1
            .into_iter()
            .collect::<std::collections::HashSet<_>>()
        {
            println!("\t{} ({})", mod_id, mod_name_map[&mod_id]);
        }
    }
    Ok(())
}

fn find_mod_assets<P: AsRef<Path>>(path: P) -> Result<Vec<String>> {
    let pak = repak::PakReader::new_any(BufReader::new(File::open(path)?), None)?;
    let mount_point = Path::new(pak.mount_point());
    let files = pak
        .files()
        .into_iter()
        .map(|f| -> Result<String> {
            Ok(mount_point
                .join(f)
                .strip_prefix("../../../")?
                .with_extension("")
                .to_string_lossy()
                .to_string())
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(files)
}

fn find_pak<P: AsRef<Path>>(dir: P) -> Result<Option<PathBuf>> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            if let Some(path) = find_pak(&path)? {
                return Ok(Some(path));
            }
        } else {
            if path.extension() == Some(std::ffi::OsStr::new("pak")) {
                return Ok(Some(path.into()));
            }
        }
    }
    Ok(None)
}
