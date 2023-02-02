use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs::File,
    io::{BufRead, BufReader, Cursor, Read, Seek},
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Result};
use colored::Colorize;
use repak::PakReader;
use unreal_asset::{reader::asset_trait::AssetTrait, Asset};

fn main() -> Result<()> {
    // https://github.com/mackwic/colored/issues/110
    #[cfg(windows)]
    {
        let _varname = colored::control::set_virtual_terminal(true).unwrap_or(());
    }

    if let Some(path) = std::env::args().nth(1) {
        let mut pak = get_pak(path)?;
        let mount_point = PathBuf::from(pak.mount_point());
        if let Ok(sanitized) = mount_point.strip_prefix("../../../") {
            let valid_extensions =
                vec!["uasset", "uexp", "umap", "ubulk", "ufont", "ini", "locres"]
                    .into_iter()
                    .collect::<HashSet<_>>();
            let mut extraneous_files = HashSet::new();
            let mut extensions = HashMap::new();
            for f in pak.files() {
                let path = Path::new(&f);
                let no_ext = String::from(path.with_extension("").to_string_lossy());
                if let Some(ext) = path.extension() {
                    let ext = String::from(ext.to_string_lossy());
                    if !valid_extensions.contains(ext.as_str()) {
                        extraneous_files.insert(f);
                    }
                    extensions
                        .entry(no_ext)
                        .or_insert(HashSet::new())
                        .insert(ext);
                } else {
                    extraneous_files.insert(f);
                }
            }
            let extraneous_files = extraneous_files
                .into_iter()
                .map(|f| String::from(sanitized.join(f).to_string_lossy()))
                .filter(|f| f != "FSD/AssetRegistry.bin")
                .collect::<BTreeSet<_>>();
            if !extraneous_files.is_empty() {
                println!("{}", "extraneous files:".bold());
                for f in extraneous_files {
                    println!("\t{}", f);
                }
            }
            let mut split_pairs = BTreeSet::new();
            let mut asset_types = BTreeMap::new();
            for (f, ext) in extensions {
                let uasset = ext.contains("uasset");
                let umap = ext.contains("umap");
                let uexp = ext.contains("uexp");
                if (umap || uasset) != uexp {
                    for e in ext {
                        split_pairs.insert(String::from(
                            sanitized.join(&f).with_extension(e).to_string_lossy(),
                        ));
                    }
                } else if (umap || uasset) && uexp {
                    let uasset = pak.get(&if uasset {
                        format!("{}.uasset", f)
                    } else {
                        format!("{}.umap", f)
                    })?;
                    let uexp = pak.get(&format!("{}.uexp", f))?;
                    let result = std::panic::catch_unwind(|| {
                        let mut asset = unreal_asset::Asset::new(uasset, Some(uexp));
                        asset.set_engine_version(
                            unreal_asset::engine_version::EngineVersion::VER_UE4_27,
                        );
                        asset
                            .parse_data()
                            .map_err(|_| anyhow!("failed to parse asset"))
                            .and_then(|_| get_type(&asset))
                    })
                    .map_err(|_| anyhow!("failed to parse asset"));
                    asset_types.insert(
                        String::from(sanitized.join(f).to_string_lossy()),
                        //result.map_or_else(|e| e, |e| Box::new(e)),
                        result.and_then(|e| e),
                    );
                }
            }
            if !split_pairs.is_empty() {
                println!("{}", "split asset pairs:".bold());
                for f in split_pairs {
                    println!("\t{}", f);
                }
            }
            if !asset_types.is_empty() {
                let auto_verified = [
                    "SoundWave",
                    "SoundCue",
                    "SoundClass",
                    "SoundMix",
                    "MaterialInstanceConstant",
                    "Material",
                    "SkeletalMesh",
                    "StaticMesh",
                    "Texture2D",
                    "AnimSequence",
                    "Skeleton",
                    "StringTable",
                ]
                .into_iter()
                .collect::<HashSet<_>>();

                let mut auto_verified_results = asset_types
                    .into_iter()
                    .map(|(f, t)| {
                        let auto_verify = match &t {
                            Ok(t) => {
                                if auto_verified.contains(t.as_str()) {
                                    AutoVerify::Pass
                                } else {
                                    AutoVerify::Fail
                                }
                            }
                            _ => AutoVerify::Unknown,
                        };
                        let msg = match t {
                            Ok(t) => AssetType::Known(t),
                            Err(e) => AssetType::Unknown(format!("{}", e)),
                        };
                        (auto_verify, msg, f)
                    })
                    .collect::<Vec<_>>();

                auto_verified_results.sort();

                println!(
                    "{:12} {:30} {}",
                    "auto-verify".bold(),
                    "class".bold(),
                    "asset path".bold()
                );
                for (a, m, f) in auto_verified_results {
                    println!("{:^12} {:30} {}", a.output(), m.output(), f);
                }
            }
        } else {
            return Err(anyhow!(
                "Invalid mount point: {}, should begin with \"../../../\"",
                pak.mount_point()
            ));
        }
    } else {
        println!("Usage: {} <mod .pak or .zip>", env!("CARGO_BIN_NAME"))
    }
    Ok(())
}

#[derive(Debug, Ord, Eq, PartialEq, PartialOrd)]
enum AutoVerify {
    Pass,
    Fail,
    Unknown,
}

impl AutoVerify {
    fn output(&self) -> colored::ColoredString {
        match self {
            AutoVerify::Pass => "yes".green(),
            AutoVerify::Fail => "no".red(),
            AutoVerify::Unknown => "?".yellow(),
        }
    }
}

#[derive(Debug, Ord, Eq, PartialEq, PartialOrd)]
enum AssetType {
    Known(String),
    Unknown(String),
}

impl AssetType {
    fn output(&self) -> colored::ColoredString {
        match self {
            AssetType::Known(s) => s.normal(),
            AssetType::Unknown(s) => s.yellow(),
        }
    }
}

fn get_type(asset: &Asset) -> Result<String> {
    use unreal_asset::exports::ExportBaseTrait;

    for e in &asset.exports {
        let base = e.get_base_export();
        if base.outer_index.index == 0 {
            return Ok(asset
                .get_import(base.class_index)
                .ok_or_else(|| anyhow!("missing class import"))?
                .object_name
                .content
                .to_owned());
        }
    }
    Err(anyhow!("could not determine asset class"))
}

trait Reader: BufRead + Seek {}
impl<T> Reader for T where T: BufRead + Seek {}

fn get_pak<P: AsRef<Path>>(path: P) -> Result<PakReader<Box<dyn Reader>>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file.try_clone()?);

    match zip::ZipArchive::new(reader) {
        Ok(mut archive) => {
            for i in 0..archive.len() {
                let mut file = archive.by_index(i)?;
                if file.is_file() && file.name().to_lowercase().ends_with(".pak") {
                    let mut buffer: Vec<u8> = vec![];
                    file.read_to_end(&mut buffer)?;
                    let reader: Box<dyn Reader> = Box::new(Cursor::new(buffer));
                    return Ok(PakReader::new_any(reader, None)?);
                }
            }
            Err(anyhow!("no pak found in zip"))
        }
        _ => {
            let reader: Box<dyn Reader> = Box::new(BufReader::new(file));
            Ok(PakReader::new_any(reader, None)?)
        }
    }
}
